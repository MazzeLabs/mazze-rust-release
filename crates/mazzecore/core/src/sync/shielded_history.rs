// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Shielded history backfill.
//!
//! Every node keeps the executed shielded sections forever (the
//! `ShieldedHistory` column): wallets need them to rebuild the tree, see
//! spends and restore from the seed. A node records them from the first
//! epoch it executes; a node that started from a snapshot has nothing
//! below it, so it fetches that range from peers whose history is complete.
//!
//! Nothing a peer sends is trusted. Replaying the sections from the empty
//! tree must reproduce, after every applied transaction, a root that the
//! state recorded at exactly that epoch, and every nullifier must be spent
//! in the state. Commitments, positions and nullifiers are thereby bound to
//! the chain. A peer that leaves sections out is caught at the end: the
//! replayed tree, continued over the node's own history, must equal the
//! state's. On any failure the range is fetched again from other peers.
//!
//! What the state does not bind (the recovery slot, public amounts) is
//! covered by the slot's MAC under the owner's key: a peer can make a
//! restore fail, never make it wrong.

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use crate::{
    block_data_manager::ShieldedHistoryEntry,
    message::Message,
    sync::{
        message::{GetShieldedHistory, ShieldedHistory},
        SynchronizationProtocolHandler,
    },
};
use mazze_executor::{
    shielded_engine::{nullifier_spent, root_height, tree_state, ShieldedOutcome},
    spec::ShieldedParams,
    state::State,
};
use mazze_shielded_pq::{
    field::{from_bytes, DIGEST},
    tree::Frontier,
};
use mazze_types::H256;
use network::{node_table::NodeId, NetworkContext};
use parking_lot::Mutex;
use rand::seq::SliceRandom;

/// Epochs one request covers. At most `1000 / slot_epochs` sections of up
/// to `max_txs_per_block` 441-byte bodies: a few hundred kilobytes.
pub const MAX_HISTORY_EPOCHS_PER_REQUEST: u64 = 1000;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// One recorded section: its epoch, its pivot block, its bodies and
/// outcomes.
pub type HistoryRow = (u64, H256, ShieldedHistoryEntry);

/// The first slot epoch of the lane, if it is configured.
pub fn first_slot(params: &ShieldedParams) -> Option<u64> {
    if params.activation_height == u64::MAX || params.slot_epochs == 0 {
        return None;
    }
    let slot = params.slot_epochs;
    Some(params.activation_height.div_ceil(slot) * slot)
}

/// Appends the applied outputs of `entry`, the section of slot `epoch`, to
/// `frontier`, and returns the entry reduced to its applied transactions
/// (all a wallet needs, and all the state can vouch for). With `state`,
/// every step is checked against it.
pub fn replay_entry(
    frontier: &mut Frontier, epoch: u64, entry: &ShieldedHistoryEntry,
    state: Option<&State>,
) -> Result<ShieldedHistoryEntry, String> {
    let mut applied = ShieldedHistoryEntry { bodies: Vec::new(), outcomes: Vec::new() };
    for (body, outcome) in entry.bodies.iter().zip(&entry.outcomes) {
        let positions = match outcome {
            ShieldedOutcome::Applied { positions } => *positions,
            ShieldedOutcome::Skipped(_) => continue,
        };
        if positions != [frontier.leaves, frontier.leaves + 1] {
            return Err(format!(
                "epoch {epoch}: positions {positions:?}, expected {}",
                frontier.leaves
            ));
        }
        for cm in &body.cm_out {
            let leaf = from_bytes::<DIGEST>(cm.as_bytes())
                .ok_or_else(|| format!("epoch {epoch}: malformed commitment"))?;
            frontier
                .append(leaf)
                .ok_or_else(|| format!("epoch {epoch}: tree full"))?;
        }
        if let Some(state) = state {
            let recorded = root_height(state, &frontier.root())
                .map_err(|e| format!("state: {e:?}"))?;
            if recorded != Some(epoch) {
                return Err(format!(
                    "epoch {epoch}: the root after {:?} was recorded at {recorded:?}",
                    body.hash()
                ));
            }
            for nf in &body.nf {
                if !nullifier_spent(state, nf).map_err(|e| format!("state: {e:?}"))? {
                    return Err(format!("epoch {epoch}: nullifier not spent"));
                }
            }
        }
        applied.bodies.push(body.clone());
        applied.outcomes.push(outcome.clone());
    }
    Ok(applied)
}

/// Checks a peer's answer for `[from, to)` on top of `frontier`: epochs in
/// range, slots, increasing, then a replay against `state`. Returns the
/// rows to store.
pub fn verify_response(
    frontier: &mut Frontier, from: u64, to: u64, rows: &[HistoryRow],
    params: &ShieldedParams, state: &State,
) -> Result<Vec<HistoryRow>, String> {
    let mut last = None;
    let mut out = Vec::with_capacity(rows.len());
    for (epoch, hash, entry) in rows {
        if *epoch < from || *epoch >= to || !params.is_slot(*epoch) {
            return Err(format!("epoch {epoch} outside [{from}, {to}) or not a slot"));
        }
        if last.map_or(false, |l| *epoch <= l) {
            return Err(format!("epoch {epoch} out of order"));
        }
        last = Some(*epoch);
        let applied = replay_entry(frontier, *epoch, entry, Some(state))?;
        if !applied.bodies.is_empty() {
            out.push((*epoch, *hash, applied));
        }
    }
    Ok(out)
}

#[derive(Default)]
pub struct ShieldedBackfill {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// The next epoch to fetch and the tree replayed up to it.
    replayed: Option<(u64, Frontier)>,
    outstanding: Option<Outstanding>,
    /// Peers that failed or had no complete history; cleared when none is
    /// left.
    excluded: HashSet<NodeId>,
    /// Peers whose sections were stored, blamed if the final check fails.
    served_by: HashSet<NodeId>,
}

struct Outstanding {
    peer: NodeId,
    from: u64,
    to: u64,
    since: Instant,
}

impl ShieldedBackfill {
    /// Drives the backfill: a request at a time, then the final check.
    pub fn tick(&self, h: &SynchronizationProtocolHandler, io: &dyn NetworkContext) {
        let pool = h.graph.consensus.get_tx_pool();
        let machine = pool.machine();
        let params = &machine.params().shielded;
        let data_man = &h.graph.data_man;
        let (first, start) = match (first_slot(params), data_man.shielded_history_start()) {
            (Some(first), Some(start)) if start > first => (first, start),
            _ => return,
        };
        if h.catch_up_mode() {
            return;
        }
        let mut inner = self.inner.lock();
        if inner.replayed.is_none() {
            let cursor = data_man.shielded_backfill_cursor().unwrap_or(first).clamp(first, start);
            let mut frontier = Frontier::default();
            for (epoch, _, entry) in data_man.shielded_history_range(first, cursor) {
                if replay_entry(&mut frontier, epoch, &entry, None).is_err() {
                    // Stored history that does not replay: start over.
                    data_man.delete_shielded_history_range(first, start);
                    data_man.set_shielded_backfill_cursor(first);
                    inner.replayed = Some((first, Frontier::default()));
                    return;
                }
            }
            inner.replayed = Some((cursor, frontier));
        }
        let (cursor, frontier) = inner.replayed.clone().expect("set above");

        if cursor >= start {
            let state = pool.best_executed_state_view();
            match final_check(h, frontier, start, &state) {
                Ok(()) => {
                    info!("shielded history backfilled and verified from epoch {first}");
                    data_man.set_shielded_history_start(first);
                }
                Err(e) => {
                    warn!("shielded history backfill failed its final check ({e}); refetching");
                    data_man.delete_shielded_history_range(first, start);
                    data_man.set_shielded_backfill_cursor(first);
                    let blamed = std::mem::take(&mut inner.served_by);
                    inner.excluded.extend(blamed);
                    inner.replayed = Some((first, Frontier::default()));
                }
            }
            return;
        }

        if let Some(o) = &inner.outstanding {
            if o.since.elapsed() < REQUEST_TIMEOUT {
                return;
            }
            let peer = o.peer;
            inner.excluded.insert(peer);
            inner.outstanding = None;
        }
        let peers: Vec<NodeId> = h
            .peer_ids()
            .into_iter()
            .filter(|p| !inner.excluded.contains(p))
            .collect();
        let peer = match peers.choose(&mut rand::thread_rng()) {
            Some(peer) => *peer,
            None => {
                inner.excluded.clear();
                return;
            }
        };
        let to = (cursor + MAX_HISTORY_EPOCHS_PER_REQUEST).min(start);
        let msg = GetShieldedHistory { from: cursor, to };
        if let Err(e) = msg.send(io, &peer) {
            debug!("requesting shielded history failed: {:?}", e);
            return;
        }
        inner.outstanding = Some(Outstanding { peer, from: cursor, to, since: Instant::now() });
    }

    /// A peer's answer to the outstanding request.
    pub fn on_response(
        &self, h: &SynchronizationProtocolHandler, io: &dyn NetworkContext,
        peer: &NodeId, msg: ShieldedHistory,
    ) {
        {
            let mut inner = self.inner.lock();
            match &inner.outstanding {
                Some(o) if o.peer == *peer && o.from == msg.from && o.to == msg.to => {}
                _ => return,
            }
            inner.outstanding = None;
            if !msg.complete {
                inner.excluded.insert(*peer);
                return;
            }
            let pool = h.graph.consensus.get_tx_pool();
            let machine = pool.machine();
            let params = &machine.params().shielded;
            let state = pool.best_executed_state_view();
            let (_, mut frontier) = match inner.replayed.clone() {
                Some(r) => r,
                None => return,
            };
            match verify_response(&mut frontier, msg.from, msg.to, &msg.entries, params, &state) {
                Ok(rows) => {
                    let data_man = &h.graph.data_man;
                    for (epoch, hash, entry) in &rows {
                        data_man.insert_shielded_history(*epoch, hash, entry);
                    }
                    data_man.set_shielded_backfill_cursor(msg.to);
                    inner.replayed = Some((msg.to, frontier));
                    inner.served_by.insert(*peer);
                }
                Err(e) => {
                    warn!("shielded history from {:?} refused: {}", peer, e);
                    inner.excluded.insert(*peer);
                    return;
                }
            }
        }
        self.tick(h, io);
    }
}

/// The replayed tree, continued over this node's own history up to the
/// last root the state recorded, must be the state's tree.
fn final_check(
    h: &SynchronizationProtocolHandler, mut frontier: Frontier, start: u64,
    state: &State,
) -> Result<(), String> {
    let (root, leaves) = tree_state(state).map_err(|e| format!("state: {e:?}"))?;
    let last = root_height(state, &root).map_err(|e| format!("state: {e:?}"))?;
    if let Some(last) = last.filter(|&l| l >= start) {
        for (epoch, hash, entry) in h.graph.data_man.shielded_history_range(start, last + 1) {
            let pivot = h
                .graph
                .consensus
                .get_hash_from_epoch_number(primitives::EpochNumber::Number(epoch))
                .ok();
            if pivot.map_or(false, |p| p != hash) {
                continue;
            }
            replay_entry(&mut frontier, epoch, &entry, None)?;
        }
    }
    if frontier.leaves != leaves || frontier.root() != root {
        return Err(format!("{} leaves replayed, the state has {leaves}", frontier.leaves));
    }
    Ok(())
}

/// What a node with complete history answers for `[from, to)`: the
/// sections executed for the current pivots.
pub fn serve(h: &SynchronizationProtocolHandler, from: u64, to: u64) -> ShieldedHistory {
    let pool = h.graph.consensus.get_tx_pool();
    let machine = pool.machine();
    let params = &machine.params().shielded;
    let data_man = &h.graph.data_man;
    let complete = match (first_slot(params), data_man.shielded_history_start()) {
        (Some(first), Some(start)) => start <= first,
        _ => false,
    };
    let to = to
        .min(from.saturating_add(MAX_HISTORY_EPOCHS_PER_REQUEST))
        .min(h.graph.consensus.best_executed_state_epoch_number() + 1);
    if !complete || from >= to {
        return ShieldedHistory { from, to, complete, entries: Vec::new() };
    }
    let rows = data_man.shielded_history_range(from, to);
    let mut entries: Vec<HistoryRow> = Vec::new();
    for (epoch, hash, entry) in rows {
        let pivot = h
            .graph
            .consensus
            .get_hash_from_epoch_number(primitives::EpochNumber::Number(epoch))
            .ok();
        let keep = match pivot {
            Some(pivot) => pivot == hash,
            // Below this node's own anchor only backfilled pivots exist.
            None => !entries.iter().any(|(e, _, _)| *e == epoch),
        };
        if keep {
            entries.push((epoch, hash, entry));
        }
    }
    ShieldedHistory { from, to, complete, entries }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mazze_executor::shielded_engine::SkipReason;
    use primitives::shielded::ShieldedTxBody;

    fn applied(seed: u32, at: u64) -> (ShieldedTxBody, ShieldedOutcome) {
        (ShieldedTxBody::sample_for_tests(seed), ShieldedOutcome::Applied { positions: [at, at + 1] })
    }

    fn entry(txs: Vec<(ShieldedTxBody, ShieldedOutcome)>) -> ShieldedHistoryEntry {
        let (bodies, outcomes) = txs.into_iter().unzip();
        ShieldedHistoryEntry { bodies, outcomes }
    }

    #[test]
    fn replay_follows_positions_and_drops_skipped() {
        let mut f = Frontier::default();
        let skipped = (ShieldedTxBody::sample_for_tests(9), ShieldedOutcome::Skipped(SkipReason::Spent));
        let e = entry(vec![applied(1, 0), skipped, applied(2, 2)]);
        let kept = replay_entry(&mut f, 10, &e, None).unwrap();
        assert_eq!(f.leaves, 4);
        assert_eq!(kept.bodies.len(), 2, "skipped transactions are dropped");
        // A gap (a section left out) shows up as wrong positions.
        let mut g = Frontier::default();
        assert!(replay_entry(&mut g, 10, &entry(vec![applied(2, 2)]), None).is_err());
    }

    #[test]
    fn first_slot_rounds_up_to_a_slot() {
        let p = |activation_height, slot_epochs| ShieldedParams {
            activation_height,
            slot_epochs,
            ..Default::default()
        };
        assert_eq!(first_slot(&p(0, 10)), Some(0));
        assert_eq!(first_slot(&p(15, 10)), Some(20));
        assert_eq!(first_slot(&p(20, 10)), Some(20));
        assert_eq!(first_slot(&p(u64::MAX, 10)), None);
    }

    /// Sections executed by the engine, then checked the way a peer's
    /// answer is: the honest history passes and ends at the state's tree;
    /// any tampering or a missing section in the middle is refused; a
    /// missing last section passes the replay but not the final comparison.
    #[test]
    fn peer_history_is_checked_against_the_state() {
        use mazze_executor::shielded_engine::{execute_section, root_height};
        use mazze_shielded_pq::field::{to_bytes, Xof};
        use mazze_statedb::StateDb;
        use mazze_storage::{
            state_manager::StateManagerTrait, tests::new_state_manager_for_unit_test,
        };
        use primitives::shielded::ShieldedDigest;
        use std::sync::Arc;

        let digest = |label: &str, i: u64| {
            let elements: [_; 12] = Xof::new(label, &[&i.to_le_bytes()]).fields();
            ShieldedDigest::from_bytes(&to_bytes(&elements)).unwrap()
        };
        let params = ShieldedParams {
            activation_height: 0,
            slot_epochs: 10,
            anchor_window_epochs: 1000,
            tx_fee: 0,
            ..Default::default()
        };
        let sm = new_state_manager_for_unit_test();
        let mut state = State::new(StateDb::new(sm.get_state_for_genesis_write())).unwrap();

        let mut rows: Vec<HistoryRow> = Vec::new();
        let mut seed = 0u32;
        for epoch in [10u64, 20, 30] {
            let mut bodies = Vec::new();
            for _ in 0..2 {
                seed += 1;
                let mut b = ShieldedTxBody::sample_for_tests(seed);
                b.anchor = ShieldedDigest::from_bytes(&to_bytes(
                    &mazze_executor::shielded_engine::tree_state(&state).unwrap().0,
                ))
                .unwrap();
                b.nf = [digest("nf", 2 * seed as u64), digest("nf", 2 * seed as u64 + 1)];
                b.cm_out = [digest("cm", 2 * seed as u64), digest("cm", 2 * seed as u64 + 1)];
                b.v_public_in = 0;
                b.v_public_out = 0;
                b.dest = None;
                b.fee = 0;
                bodies.push(b);
            }
            let arcs: Vec<_> = bodies.iter().cloned().map(Arc::new).collect();
            let outcomes = execute_section(&mut state, &arcs, epoch, &params).unwrap();
            assert!(outcomes.iter().all(|o| matches!(o, ShieldedOutcome::Applied { .. })));
            rows.push((epoch, H256::from_low_u64_be(epoch), ShieldedHistoryEntry { bodies, outcomes }));
        }
        let (root, leaves) = tree_state(&state).unwrap();
        assert_eq!(root_height(&state, &root).unwrap(), Some(30));

        let check = |rows: &[HistoryRow]| {
            let mut f = Frontier::default();
            verify_response(&mut f, 0, 1000, rows, &params, &state).map(|_| f)
        };
        let honest = check(&rows).expect("honest history verifies");
        assert_eq!((honest.leaves, honest.root()), (leaves, root));

        let mut wrong_cm = rows.clone();
        wrong_cm[1].2.bodies[0].cm_out[1] = digest("evil", 1);
        assert!(check(&wrong_cm).is_err(), "a changed commitment");

        let mut wrong_nf = rows.clone();
        wrong_nf[2].2.bodies[1].nf[0] = digest("evil", 2);
        assert!(check(&wrong_nf).is_err(), "a nullifier the state never saw");

        let mut wrong_epoch = rows.clone();
        wrong_epoch[0].0 = 0;
        assert!(check(&wrong_epoch).is_err(), "a section moved to another slot");

        let gap = vec![rows[0].clone(), rows[2].clone()];
        assert!(check(&gap).is_err(), "a section left out in the middle");

        let truncated = check(&rows[..2]).expect("a prefix replays");
        assert_ne!(truncated.leaves, leaves, "the final comparison catches it");
    }
}
