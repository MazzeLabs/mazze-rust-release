// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Dandelion++ routing for shielded transactions (spec §9.6).
//!
//! A transaction first travels a *stem*: one peer at a time, each node
//! forwarding to its current stem successor, until a node switches it to
//! *fluff* (ordinary broadcast) with probability `FLUFF_PROBABILITY`. A node
//! that relays a stem transaction sets an *embargo*: if the transaction is
//! not seen fluffing before the deadline (a peer swallowed it), the node
//! fluffs it itself. The node where the transaction originated is then not
//! the first to broadcast it, which defeats "first spy" origin detection.
//! It does not defeat an observer of the node's own uplink (see the spec).

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use mazze_types::H256;
use network::node_table::NodeId;
use parking_lot::Mutex;
use rand::Rng;

/// Probability that a stem hop switches to fluff.
pub const FLUFF_PROBABILITY: f64 = 0.1;
/// How long a stem successor is kept before a new one is drawn.
const SUCCESSOR_LIFETIME: Duration = Duration::from_secs(600);
/// Minimum embargo; a random part of up to the same length is added.
const EMBARGO_BASE: Duration = Duration::from_secs(30);
/// Fluffed hashes remembered, to avoid re-broadcasting.
const FLUFFED_MEMORY: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    /// Send to this peer only, as a stem.
    Stem(NodeId),
    /// Broadcast to every peer except the sender.
    Fluff,
    /// Already handled.
    Drop,
}

#[derive(Default)]
pub struct Dandelion {
    successor: Mutex<Option<(NodeId, Instant)>>,
    embargo: Mutex<HashMap<H256, Instant>>,
    fluffed: Mutex<(HashSet<H256>, Vec<H256>)>,
}

impl Dandelion {
    /// The current stem successor among `peers`, redrawn when it expired or
    /// left. `exclude` (the peer a stem came from) is never chosen, so a
    /// stem does not bounce straight back.
    fn successor(&self, peers: &[NodeId], exclude: Option<&NodeId>) -> Option<NodeId> {
        let mut current = self.successor.lock();
        let valid = |id: &NodeId| peers.contains(id) && Some(id) != exclude;
        if let Some((id, since)) = *current {
            if valid(&id) && since.elapsed() < SUCCESSOR_LIFETIME {
                return Some(id);
            }
        }
        let candidates: Vec<&NodeId> = peers.iter().filter(|id| valid(id)).collect();
        if candidates.is_empty() {
            return None;
        }
        let id = *candidates[rand::thread_rng().gen_range(0, candidates.len())];
        *current = Some((id, Instant::now()));
        Some(id)
    }

    fn embargo_until(&self, hash: H256) {
        let jitter = rand::thread_rng().gen_range(0, EMBARGO_BASE.as_millis() as u64);
        self.embargo
            .lock()
            .entry(hash)
            .or_insert(Instant::now() + EMBARGO_BASE + Duration::from_millis(jitter));
    }

    /// Records that `hash` is fluffing. Returns false if it already was.
    pub fn note_fluffed(&self, hash: H256) -> bool {
        self.embargo.lock().remove(&hash);
        let mut fluffed = self.fluffed.lock();
        if !fluffed.0.insert(hash) {
            return false;
        }
        fluffed.1.push(hash);
        if fluffed.1.len() > FLUFFED_MEMORY {
            let old = fluffed.1.remove(0);
            fluffed.0.remove(&old);
        }
        true
    }

    /// Route for a transaction this node originated: always a stem when a
    /// peer exists.
    pub fn route_originated(&self, hash: H256, peers: &[NodeId]) -> Route {
        match self.successor(peers, None) {
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

    /// Route for a stem transaction received from `from`.
    pub fn route_stem(&self, hash: H256, from: &NodeId, peers: &[NodeId]) -> Route {
        if self.fluffed.lock().0.contains(&hash) {
            return Route::Drop;
        }
        if rand::thread_rng().gen_bool(FLUFF_PROBABILITY) {
            self.note_fluffed(hash);
            return Route::Fluff;
        }
        match self.successor(peers, Some(from)) {
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

    #[test]
    fn originated_transactions_stem_to_one_stable_successor() {
        let d = Dandelion::default();
        let p = peers(5);
        let first = d.route_originated(H256::repeat_byte(1), &p);
        let second = d.route_originated(H256::repeat_byte(2), &p);
        assert!(matches!(first, Route::Stem(_)));
        assert_eq!(first, second, "one successor per period");
        assert_eq!(d.route_originated(H256::repeat_byte(3), &[]), Route::Fluff);
    }

    #[test]
    fn stems_mostly_continue_and_never_bounce_back() {
        let d = Dandelion::default();
        let p = peers(3);
        let from = p[0];
        let mut fluffs = 0;
        for i in 0..2000u64 {
            match d.route_stem(H256::from_low_u64_be(i), &from, &p) {
                Route::Stem(peer) => assert_ne!(peer, from),
                Route::Fluff => fluffs += 1,
                Route::Drop => panic!("new hash dropped"),
            }
        }
        assert!((100..320).contains(&fluffs), "fluff rate ~10%, got {fluffs}/2000");
    }

    #[test]
    fn fluff_is_broadcast_once() {
        let d = Dandelion::default();
        let h = H256::repeat_byte(9);
        assert_eq!(d.route_fluff(h), Route::Fluff);
        assert_eq!(d.route_fluff(h), Route::Drop);
        assert_eq!(d.route_stem(h, &peers(1)[0], &peers(3)), Route::Drop);
    }

    #[test]
    fn embargo_expires_into_a_fluff() {
        let d = Dandelion::default();
        let h = H256::repeat_byte(4);
        d.embargo.lock().insert(h, Instant::now() - Duration::from_secs(1));
        assert_eq!(d.expired_embargoes(), vec![h]);
        assert!(d.expired_embargoes().is_empty());
        assert_eq!(d.route_fluff(h), Route::Drop, "already fluffed");
    }
}
