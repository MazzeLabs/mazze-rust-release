// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Dandelion++ routing for shielded transactions (spec §9.6), following
//! Fanti et al., "Dandelion++" (2018).
//!
//! Time is cut into epochs. In each one a node is a *diffuser* with
//! probability `DIFFUSER_PROBABILITY`, broadcasting (fluffing) every stem
//! transaction it receives, or else a *relayer*, forwarding them along the
//! stem. A relayer has two stem successors, drawn **only among outbound
//! peers** (connections it opened: an attacker can open many inbound ones,
//! but cannot make the node connect to it), and maps each inbound edge to one
//! of them for the whole epoch, so the graph of stems is a fixed line per
//! epoch rather than a fresh random walk per transaction. The node's own
//! transactions always take the first successor.
//!
//! Every node holding a stem transaction arms an *embargo* timer: if the
//! transaction is not seen fluffing by then (a peer swallowed it), the node
//! fluffs it itself. The timer is exponential, hence memoryless: whoever
//! got the transaction first, every node on the stem is about equally likely
//! to fire first, so the originator is not the one that betrays itself.
//!
//! It does not defeat an observer of the node's own uplink (see the spec).

use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

use keccak_hash::keccak;
use mazze_types::H256;
use network::node_table::NodeId;
use parking_lot::Mutex;
use rand::Rng;

/// Probability that a node is a diffuser for an epoch.
pub const DIFFUSER_PROBABILITY: f64 = 0.1;
/// Length of an epoch: successors and the node's role are fixed within it.
const EPOCH: Duration = Duration::from_secs(600);
/// Stem successors per epoch.
const SUCCESSORS: usize = 2;
/// The embargo: a fixed part plus an exponential one of this mean.
const EMBARGO_BASE: Duration = Duration::from_secs(10);
const EMBARGO_MEAN_SECS: f64 = 30.0;
/// Hashes remembered as fluffed, and as already routed in a stem.
const MEMORY: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    /// Send to this peer only, as a stem.
    Stem(NodeId),
    /// Broadcast to every peer except the sender.
    Fluff,
    /// Already seen fluffing: public, nothing to send.
    Drop,
    /// A copy of a stem already routed: nothing to send, and nothing learned
    /// (it says nothing about the transaction being public).
    Ignore,
    /// No stem successor yet: keep the transaction and try again later.
    Wait,
}

struct Epoch {
    started: Instant,
    diffuser: bool,
    successors: Vec<NodeId>,
    /// Keys the fixed map from inbound edges to successors.
    salt: [u8; 16],
}

/// A bounded set of recently seen hashes.
#[derive(Default)]
struct Recent {
    set: HashSet<H256>,
    order: VecDeque<H256>,
}

impl Recent {
    fn insert(&mut self, hash: H256) -> bool {
        if !self.set.insert(hash) {
            return false;
        }
        self.order.push_back(hash);
        if self.order.len() > MEMORY {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }

    fn contains(&self, hash: &H256) -> bool { self.set.contains(hash) }
}

#[derive(Default)]
pub struct Dandelion {
    epoch: Mutex<Option<Epoch>>,
    embargo: Mutex<HashMap<H256, Instant>>,
    fluffed: Mutex<Recent>,
    stemmed: Mutex<Recent>,
}

impl Dandelion {
    /// The current epoch, renewed when it expired, and its successors kept
    /// among the connected `outbound` peers (refilled when one left).
    fn with_epoch<T>(&self, outbound: &[NodeId], f: impl FnOnce(&Epoch) -> T) -> T {
        let mut guard = self.epoch.lock();
        let mut rng = rand::thread_rng();
        let expired = guard.as_ref().map_or(true, |e| e.started.elapsed() >= EPOCH);
        if expired {
            *guard = Some(Epoch {
                started: Instant::now(),
                diffuser: rng.gen_bool(DIFFUSER_PROBABILITY),
                successors: Vec::new(),
                salt: rng.gen(),
            });
        }
        let epoch = guard.as_mut().expect("set above");
        epoch.successors.retain(|s| outbound.contains(s));
        while epoch.successors.len() < SUCCESSORS {
            let candidates: Vec<&NodeId> =
                outbound.iter().filter(|p| !epoch.successors.contains(p)).collect();
            if candidates.is_empty() {
                break;
            }
            let pick = *candidates[rng.gen_range(0, candidates.len())];
            epoch.successors.push(pick);
        }
        f(epoch)
    }

    fn embargo_until(&self, hash: H256) {
        let u: f64 = rand::thread_rng().gen_range(f64::MIN_POSITIVE, 1.0);
        let exponential = Duration::from_secs_f64(-u.ln() * EMBARGO_MEAN_SECS);
        self.embargo
            .lock()
            .entry(hash)
            .or_insert(Instant::now() + EMBARGO_BASE + exponential);
    }

    /// Records that `hash` is fluffing. Returns false if it already was.
    pub fn note_fluffed(&self, hash: H256) -> bool {
        self.embargo.lock().remove(&hash);
        self.fluffed.lock().insert(hash)
    }

    /// A fluff that reached no peer: armed again, so it is retried.
    pub fn retry_later(&self, hash: H256) {
        self.fluffed.lock().set.remove(&hash);
        self.embargo_until(hash);
    }

    /// Route for a transaction this node originated: always a stem, to the
    /// epoch's first successor; `Wait` while there is none.
    pub fn route_originated(&self, hash: H256, outbound: &[NodeId]) -> Route {
        match self.with_epoch(outbound, |e| e.successors.first().copied()) {
            Some(peer) => {
                self.stemmed.lock().insert(hash);
                self.embargo_until(hash);
                Route::Stem(peer)
            }
            None => Route::Wait,
        }
    }

    /// Route for a stem transaction received from `from`: routed once
    /// (later copies are dropped, so a peer cannot draw again); fluffed by a
    /// diffuser, or when no successor other than the sender exists;
    /// otherwise forwarded to the successor `from` maps to.
    pub fn route_stem(&self, hash: H256, from: &NodeId, outbound: &[NodeId]) -> Route {
        if self.fluffed.lock().contains(&hash) {
            return Route::Drop;
        }
        if !self.stemmed.lock().insert(hash) {
            // A peer echoing a stem back must not be able to make this node
            // treat it as public and pack it near its origin.
            return Route::Ignore;
        }
        let next = self.with_epoch(outbound, |e| {
            if e.diffuser {
                return None;
            }
            let others: Vec<NodeId> =
                e.successors.iter().filter(|s| *s != from).copied().collect();
            if others.is_empty() {
                return None;
            }
            let mut key = e.salt.to_vec();
            key.extend_from_slice(from.as_bytes());
            let slot = keccak(&key).to_low_u64_be() as usize % others.len();
            Some(others[slot])
        });
        match next {
            Some(peer) => {
                self.embargo_until(hash);
                Route::Stem(peer)
            }
            None => {
                self.note_fluffed(hash);
                Route::Fluff
            }
        }
    }

    /// Route for a fluff transaction: broadcast once.
    pub fn route_fluff(&self, hash: H256) -> Route {
        if self.note_fluffed(hash) { Route::Fluff } else { Route::Drop }
    }

    /// Stem transactions whose embargo expired without seeing them fluff:
    /// this node fluffs them now.
    pub fn expired_embargoes(&self) -> Vec<H256> {
        let now = Instant::now();
        let mut embargo = self.embargo.lock();
        let expired: Vec<H256> = embargo
            .iter()
            .filter(|(_, deadline)| **deadline <= now)
            .map(|(h, _)| *h)
            .collect();
        for h in &expired {
            embargo.remove(h);
        }
        drop(embargo);
        for h in &expired {
            self.note_fluffed(*h);
        }
        expired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peers(n: u8) -> Vec<NodeId> { (0..n).map(|i| NodeId::from_low_u64_be(i as u64 + 1)).collect() }

    fn relayer() -> Dandelion {
        let d = Dandelion::default();
        // Force the relayer role for the test's epoch.
        d.with_epoch(&peers(3), |_| ());
        d.epoch.lock().as_mut().unwrap().diffuser = false;
        d
    }

    #[test]
    fn originated_transactions_take_one_successor_and_wait_without_one() {
        let d = Dandelion::default();
        let out = peers(5);
        let first = d.route_originated(H256::repeat_byte(1), &out);
        let second = d.route_originated(H256::repeat_byte(2), &out);
        assert!(matches!(first, Route::Stem(_)));
        assert_eq!(first, second, "one successor for the node's own txs per epoch");
        let lonely = Dandelion::default();
        assert_eq!(lonely.route_originated(H256::repeat_byte(3), &[]), Route::Wait);
        assert!(!lonely.fluffed.lock().contains(&H256::repeat_byte(3)), "nothing sent, nothing fluffed");
    }

    #[test]
    fn successors_are_outbound_and_each_inbound_edge_keeps_its_successor() {
        let d = relayer();
        let out = peers(3);
        let inbound = NodeId::from_low_u64_be(99);
        let mut seen = HashSet::new();
        for i in 0..50u64 {
            match d.route_stem(H256::from_low_u64_be(i), &inbound, &out) {
                Route::Stem(p) => {
                    assert!(out.contains(&p), "only outbound successors");
                    seen.insert(p);
                }
                other => panic!("relayer must stem, got {other:?}"),
            }
        }
        assert_eq!(seen.len(), 1, "one inbound edge maps to one successor");
    }

    #[test]
    fn a_stem_is_routed_once() {
        let d = relayer();
        let h = H256::repeat_byte(7);
        let from = NodeId::from_low_u64_be(99);
        assert!(matches!(d.route_stem(h, &from, &peers(3)), Route::Stem(_)));
        assert_eq!(d.route_stem(h, &from, &peers(3)), Route::Ignore, "a copy cannot draw again, nor make it public");
    }

    #[test]
    fn a_diffuser_fluffs_and_fluff_is_broadcast_once() {
        let d = relayer();
        d.epoch.lock().as_mut().unwrap().diffuser = true;
        let h = H256::repeat_byte(9);
        assert_eq!(d.route_stem(h, &NodeId::from_low_u64_be(99), &peers(3)), Route::Fluff);
        assert_eq!(d.route_fluff(h), Route::Drop);
        let g = H256::repeat_byte(10);
        assert_eq!(d.route_fluff(g), Route::Fluff);
        assert_eq!(d.route_fluff(g), Route::Drop);
    }

    #[test]
    fn embargoes_are_memoryless_and_expire_into_a_fluff() {
        let d = Dandelion::default();
        let deadlines: Vec<f64> = (0..2000u64)
            .map(|i| {
                let h = H256::from_low_u64_be(i);
                d.embargo_until(h);
                (d.embargo.lock()[&h] - Instant::now()).as_secs_f64()
            })
            .collect();
        let mean = deadlines.iter().sum::<f64>() / deadlines.len() as f64;
        assert!((35.0..45.0).contains(&mean), "base 10 s + exponential mean 30 s, got {mean}");
        let h = H256::repeat_byte(4);
        d.embargo.lock().insert(h, Instant::now() - Duration::from_secs(1));
        assert!(d.expired_embargoes().contains(&h));
        assert_eq!(d.route_fluff(h), Route::Drop, "already fluffed");
    }
}
