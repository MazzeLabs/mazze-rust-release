// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Fast-sync anchor bootstrap (docs/fast-sync-design.md §5.19).
//!
//! A fresh node configured with `trusted_checkpoints` does not replay the
//! chain from genesis. It puts its chain into the state of a node restarted
//! from a checkpointed database -- era genesis `R` (the root) and stable
//! checkpoint `S = R + era_epoch_count`, two consecutive trusted era
//! checkpoints -- and from there syncs like such a node: headers above the
//! root, then a snapshot of the stable checkpoint its own consensus elects,
//! then the block bodies above it.
//!
//! Why two checkpoints: the consensus validity of a block (parent choice,
//! adaptive flag, difficulty) depends on the graph below it. A restarted
//! node re-checks only the blocks in the future of its stable checkpoint,
//! which lies an era above its root, and takes the status of older blocks
//! from its database. A rooted fresh node has no such statuses, so it must
//! not re-check blocks close above its root either (it would compute them
//! against a truncated graph and flag good blocks partially invalid): the
//! blocks between `R` and `S` stay "pending" exactly as on a restarted node.
//!
//! Rooting at `R` needs what a checkpointed node keeps in its database for
//! its era genesis and which the headers above it do not tell:
//! - the headers of `R` and `S` (authenticated by the configured hashes);
//! - the epoch set of `R` (from a peer; its last block must be `R`). Only
//!   its length is consensus relevant, together with
//! - the block number of `R` (configured, `mazze_getBlockByHash`), which
//!   numbers every later block;
//! - the RandomX seeds of the epochs right above `R`: the main-chain blocks
//!   at the multiples of `RANDOMX_EPOCH_LENGTH` down to one RandomX epoch
//!   below `R`. They are read off the parent chain of `R`, authenticated by
//!   hash linkage from `R`. A wrong seed changes the PoW quality that
//!   classifies heavy and timer blocks, i.e. the ordering.

use crate::{
    block_data_manager::{BlockStatus, EpochExecutionContext, LocalBlockInfo},
    consensus::TrustedCheckpoint,
    sync::synchronization_protocol_handler::SynchronizationProtocolHandler,
};
use mazze_parameters::pow::RANDOMX_EPOCH_LENGTH;
use mazze_types::H256;
use network::NetworkContext;
use parking_lot::Mutex;
use std::time::{Duration, Instant};

/// Epochs per `GetBlockHashesByEpoch` request of the seed prefix walk.
const EPOCH_BATCH: u64 = 30;
/// Batches requested per second (answered in parallel by several peers;
/// peers throttle `GetBlockHashesByEpoch` at 10/s, run/throttling.toml).
const BATCHES_PER_TICK: u64 = 8;
/// Re-request what is still missing after this long without progress.
const RETRY_AFTER: Duration = Duration::from_secs(20);
/// Consensus sequence number given to the root. Every block below it is
/// recorded with 0, i.e. "before the checkpoint".
pub const ANCHOR_SEQ_NUM: u64 = 1;

/// The two consecutive trusted era checkpoints a fresh node roots at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorPair {
    /// Era genesis of the rooted chain.
    pub root: TrustedCheckpoint,
    /// Its stable checkpoint, `root + era_epoch_count`.
    pub stable: TrustedCheckpoint,
}

/// All pairs of consecutive era checkpoints in `anchors`, newest first.
pub fn anchor_pairs(
    anchors: &[TrustedCheckpoint], era_epoch_count: u64,
) -> Vec<AnchorPair> {
    let mut pairs = Vec::new();
    for stable in anchors {
        if let Some(root) = anchors.iter().find(|r| {
            r.checkpoint_height + era_epoch_count == stable.checkpoint_height
        }) {
            pairs.push(AnchorPair {
                root: root.clone(),
                stable: stable.clone(),
            });
        }
    }
    pairs.sort_by(|a, b| {
        b.stable.checkpoint_height.cmp(&a.stable.checkpoint_height)
    });
    pairs
}

/// Pick the pair to root at: the newest whose stable checkpoint is at
/// least two and a half eras below the peers' median epoch, so that our
/// consensus can move its stable checkpoint above it (headers two eras
/// past it) and state-sync a checkpoint whose reward epochs it validated
/// itself. Otherwise the oldest pair (the node then waits in header sync
/// until the chain is long enough, see `catch_up_completed`).
pub fn choose_anchor_pair(
    anchors: &[TrustedCheckpoint], peer_median_epoch: u64,
    era_epoch_count: u64,
) -> Option<AnchorPair> {
    let pairs = anchor_pairs(anchors, era_epoch_count);
    if let Some(p) = pairs.iter().find(|p| {
        p.stable.checkpoint_height + 2 * era_epoch_count + era_epoch_count / 2
            <= peer_median_epoch
    }) {
        return Some(p.clone());
    }
    pairs.last().cloned()
}

/// Heights below `root_height` whose main-chain block is the RandomX seed of
/// an epoch at or above `root_height`.
pub fn seed_heights_below(root_height: u64) -> Vec<u64> {
    let k = root_height / RANDOMX_EPOCH_LENGTH;
    if k == 0 {
        // Every epoch up to the root is in RandomX epoch 0 (seed =
        // genesis); the next seed height is the root's own or above.
        return Vec::new();
    }
    let mut heights = Vec::new();
    let mut h = (k - 1) * RANDOMX_EPOCH_LENGTH;
    while h < root_height {
        heights.push(h);
        h += RANDOMX_EPOCH_LENGTH;
    }
    heights
}

#[derive(Default)]
struct WalkState {
    /// Lowest header reached walking parent hashes down from the root.
    cursor: Option<(H256, u64)>,
    /// Main-chain blocks found at the seed heights.
    seeds: Vec<(u64, H256)>,
    /// Lowest epoch below the root whose hashes were requested.
    requested_down_to: Option<u64>,
    last_progress: Option<Instant>,
    last_anchor_request: Option<Instant>,
    last_batch_request: Option<Instant>,
}

#[derive(Default)]
pub struct FastSyncAnchorBootstrap {
    walk: Mutex<WalkState>,
}

impl FastSyncAnchorBootstrap {
    pub fn reset(&self) { *self.walk.lock() = WalkState::default(); }

    /// Collect what rooting at `pair` needs, requesting what is missing.
    /// Returns true once it is all persisted locally.
    pub fn collect(
        &self, pair: &AnchorPair, io: &dyn NetworkContext,
        sync_handler: &SynchronizationProtocolHandler,
    ) -> bool {
        let data_man = &sync_handler.graph.data_man;
        let mut walk = self.walk.lock();
        let now = Instant::now();
        let due = |t: Option<Instant>| {
            t.map_or(true, |t| now.duration_since(t) >= RETRY_AFTER)
        };
        let root = &pair.root;

        // 1. The two anchor headers and the root's epoch set.
        let mut missing_headers = Vec::new();
        for cp in [&pair.root, &pair.stable] {
            match data_man.block_header_by_hash(&cp.checkpoint_hash) {
                None => missing_headers.push(cp.checkpoint_hash),
                Some(h) if h.height() != cp.checkpoint_height => {
                    if due(walk.last_anchor_request) {
                        walk.last_anchor_request = Some(now);
                        error!(
                            "fast-sync anchor: checkpoint {:?} is at height \
                             {}, not the configured {}; fix \
                             trusted_checkpoints",
                            cp.checkpoint_hash,
                            h.height(),
                            cp.checkpoint_height
                        );
                    }
                    return false;
                }
                Some(_) => {}
            }
        }
        let have_epoch_set = data_man
            .executed_epoch_set_hashes_from_db(root.checkpoint_height)
            .map_or(false, |s| s.last() == Some(&root.checkpoint_hash));
        if !missing_headers.is_empty() || !have_epoch_set {
            if due(walk.last_anchor_request) {
                walk.last_anchor_request = Some(now);
                info!(
                    "fast-sync anchor: requesting {} checkpoint header(s) \
                     and the epoch of root {} (have epoch set={})",
                    missing_headers.len(),
                    root.checkpoint_height,
                    have_epoch_set
                );
                if !missing_headers.is_empty() {
                    sync_handler.request_block_headers(
                        io,
                        None,
                        missing_headers,
                        true, /* ignore_db */
                    );
                }
                if !have_epoch_set {
                    sync_handler.request_epoch_hashes_for_prefetch(
                        io,
                        vec![root.checkpoint_height],
                    );
                }
            }
            return false;
        }

        // 2. Walk the root's parent chain down to the lowest seed height.
        let seed_heights = seed_heights_below(root.checkpoint_height);
        let floor = match seed_heights.first() {
            None => return true,
            Some(h) => *h,
        };
        let (mut cur_hash, mut cur_height) = walk
            .cursor
            .unwrap_or((root.checkpoint_hash, root.checkpoint_height));
        let start_height = cur_height;
        while cur_height > floor {
            let parent = match data_man.block_header_by_hash(&cur_hash) {
                Some(h) => *h.parent_hash(),
                None => break,
            };
            let parent_header = match data_man.block_header_by_hash(&parent) {
                Some(h) => h,
                None => break,
            };
            if parent_header.height() + 1 != cur_height {
                error!(
                    "fast-sync anchor: parent {:?} of {:?} has height {}, \
                     expected {}",
                    parent,
                    cur_hash,
                    parent_header.height(),
                    cur_height - 1
                );
                return false;
            }
            cur_hash = parent;
            cur_height -= 1;
            if seed_heights.contains(&cur_height) {
                walk.seeds.push((cur_height, cur_hash));
            }
        }
        walk.cursor = Some((cur_hash, cur_height));
        if cur_height < start_height || walk.last_progress.is_none() {
            walk.last_progress = Some(now);
        }
        if cur_height <= floor {
            return true;
        }

        // 3. Request the epochs below the walk, newest first. If the walk
        // stalls (lost responses), start over right below it.
        if due(walk.last_progress) {
            walk.last_progress = Some(now);
            walk.requested_down_to = None;
        }
        if walk
            .last_batch_request
            .map_or(false, |t| now.duration_since(t) < Duration::from_secs(1))
        {
            return false;
        }
        walk.last_batch_request = Some(now);
        let mut batches = 0;
        while batches < BATCHES_PER_TICK {
            let top = match walk.requested_down_to {
                Some(lowest) if lowest <= floor => break,
                Some(lowest) => lowest - 1,
                None => cur_height - 1,
            };
            let from = top.saturating_sub(EPOCH_BATCH - 1).max(floor);
            sync_handler
                .request_epoch_hashes_for_prefetch(io, (from..=top).collect());
            walk.requested_down_to = Some(from);
            batches += 1;
        }
        if batches > 0 {
            debug!(
                "fast-sync anchor: walked down to {} (floor {}), requested \
                 epochs down to {:?}",
                cur_height, floor, walk.requested_down_to
            );
        }
        false
    }

    /// Root the local chain at `pair`. `collect` must have returned true.
    pub fn anchor(
        &self, pair: &AnchorPair,
        sync_handler: &SynchronizationProtocolHandler,
    ) -> Result<(), String> {
        let graph = &sync_handler.graph;
        let data_man = &graph.data_man;
        let root = &pair.root;
        let hash = root.checkpoint_hash;
        let height = root.checkpoint_height;
        let epoch_set = data_man
            .executed_epoch_set_hashes_from_db(height)
            .ok_or("root epoch set missing")?;
        // The root is the last block of its epoch: its block number is
        // start_block_number + |epoch| - 1 (see `with_era_genesis`).
        let start_block_number = (root.block_number + 1)
            .checked_sub(epoch_set.len() as u64)
            .ok_or_else(|| {
                format!(
                    "configured block number {} of checkpoint {:?} is \
                     smaller than its epoch ({} blocks)",
                    root.block_number,
                    hash,
                    epoch_set.len()
                )
            })?;
        let seeds = self.walk.lock().seeds.clone();
        if seeds.len() != seed_heights_below(height).len() {
            return Err(format!(
                "seed walk incomplete: {:?} for root height {}",
                seeds, height
            ));
        }
        for (h, seed) in &seeds {
            info!("fast-sync anchor: RandomX seed at height {}: {:?}", h, seed);
            data_man.db_manager.insert_anchored_seed_hash_to_db(*h, seed);
        }
        data_man.insert_epoch_execution_context(
            hash,
            EpochExecutionContext { start_block_number },
            true, /* persistent */
        );
        data_man.insert_local_block_info(
            &hash,
            LocalBlockInfo::new(
                BlockStatus::Valid,
                ANCHOR_SEQ_NUM,
                data_man.get_instance_id(),
            ),
        );
        data_man.set_cur_consensus_era_genesis_hash(
            &hash,
            &pair.stable.checkpoint_hash,
            height,
        );
        if data_man.get_cur_consensus_era_genesis_hash() != hash {
            return Err("the database refused the root checkpoint".into());
        }
        graph.consensus.reset();
        graph.consensus.set_initial_sequence_number(ANCHOR_SEQ_NUM);
        graph.reset_to_cur_era_genesis();
        *graph.fast_sync_anchor.lock() = None;
        info!(
            "fast-sync anchor: consensus rooted at checkpoint height={} \
             hash={:?} block_number={} (epoch of {} blocks, {} seed(s)), \
             stable checkpoint height={} hash={:?}",
            height,
            hash,
            root.block_number,
            epoch_set.len(),
            seeds.len(),
            pair.stable.checkpoint_height,
            pair.stable.checkpoint_hash,
        );
        Ok(())
    }
}

/// Compare the block numbers our consensus gives the configured checkpoints
/// with the configured ones. A rooted node numbers every block from its
/// root's configured number, so a mismatch at a later checkpoint means a
/// wrong `block_number` (or epoch) in `trusted_checkpoints`: every block
/// number, and so every executed state, would differ from the network's.
pub fn check_anchor_block_numbers(
    sync_handler: &SynchronizationProtocolHandler,
) {
    let consensus = &sync_handler.graph.consensus;
    for cp in &consensus.get_config().trusted_checkpoints {
        match consensus.get_block_number(&cp.checkpoint_hash) {
            Ok(Some(n)) if n == cp.block_number => info!(
                "fast-sync anchor: block number of checkpoint {} matches \
                 the configured {}",
                cp.checkpoint_height, n
            ),
            Ok(Some(n)) => error!(
                "fast-sync anchor: checkpoint {} {:?} has block number {} \
                 here but {} in trusted_checkpoints; the configured numbers \
                 are inconsistent and executed states will not match the \
                 network",
                cp.checkpoint_height, cp.checkpoint_hash, n, cp.block_number
            ),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mazze_parameters::pow::RANDOMX_EPOCH_LENGTH as L;

    fn cp(height: u64) -> TrustedCheckpoint {
        TrustedCheckpoint {
            checkpoint_height: height,
            checkpoint_hash: H256::from_low_u64_be(height),
            block_number: height * 2,
            blame: None,
        }
    }

    fn pair(root: u64, stable: u64) -> AnchorPair {
        AnchorPair {
            root: cp(root),
            stable: cp(stable),
        }
    }

    #[test]
    fn seed_heights() {
        assert!(seed_heights_below(0).is_empty());
        assert!(seed_heights_below(L - 1).is_empty());
        // Era == RandomX epoch: one seed (the previous boundary); the root
        // itself seeds the epoch after next (its own epoch set).
        assert_eq!(seed_heights_below(28 * L), vec![27 * L]);
        // Root inside a RandomX epoch: the epoch's start and the one
        // before it.
        assert_eq!(seed_heights_below(28 * L + 500), vec![27 * L, 28 * L]);
        assert_eq!(seed_heights_below(L), vec![0]);
    }

    #[test]
    fn pair_choice() {
        let era = 20000;
        // Sorted newest-first, as the config keeps them.
        let anchors = vec![cp(580000), cp(560000), cp(540000), cp(500000)];
        assert_eq!(
            anchor_pairs(&anchors, era),
            vec![pair(560000, 580000), pair(540000, 560000)]
        );
        // Tip far enough above the newest stable.
        assert_eq!(
            choose_anchor_pair(&anchors, 640000, era).unwrap(),
            pair(560000, 580000)
        );
        // Too close to the newest: the older pair.
        assert_eq!(
            choose_anchor_pair(&anchors, 625000, era).unwrap(),
            pair(540000, 560000)
        );
        // Both too new: the oldest pair.
        assert_eq!(
            choose_anchor_pair(&anchors, 590000, era).unwrap(),
            pair(540000, 560000)
        );
        // No consecutive pair.
        assert!(choose_anchor_pair(&[cp(580000), cp(540000)], 900000, era)
            .is_none());
        assert!(choose_anchor_pair(&[], 1, era).is_none());
    }
}
