// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! The shielded lane of the transaction pool. Shielded transactions have no
//! sender, nonce or gas, so they live apart from the account transactions:
//! keyed by body hash, with an index of the nullifiers they spend.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use mazze_types::H256;
use parking_lot::RwLock;
use primitives::shielded::{ShieldedDigest, ShieldedTxBody};
use rand::seq::SliceRandom;

/// A shielded transaction waiting to be packed: its body and its proof.
#[derive(Clone, Debug)]
pub struct PooledShieldedTx {
    pub hash: H256,
    pub body: Arc<ShieldedTxBody>,
    pub proof: Arc<Vec<u8>>,
}

impl PooledShieldedTx {
    pub fn new(body: ShieldedTxBody, proof: Vec<u8>) -> Self {
        PooledShieldedTx {
            hash: body.hash(),
            body: Arc::new(body),
            proof: Arc::new(proof),
        }
    }
}

/// Who hands the pool a shielded transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShieldedSource {
    /// A wallet, through the node's RPC: it gets an answer.
    Rpc,
    /// A peer, in its Dandelion++ stem phase.
    PeerStem,
    /// A peer, broadcasting it.
    PeerFluff,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ShieldedPoolError {
    /// The lane is not active.
    Inactive,
    Duplicate,
    /// A pooled transaction already spends one of its nullifiers.
    Conflict,
    Full,
    WrongFee { expected: u64, found: u64 },
    InvalidProof(String),
    Spent,
    UnknownAnchor,
    /// A shield-in whose deposit is not (yet) credited by the bridge.
    NoDeposit,
    /// Refused for a reason that depends on the pool's contents. Given to
    /// RPC callers in place of `Conflict` and `Full`, which would tell them
    /// what the pool holds.
    NotAccepted,
}

#[derive(Default)]
struct Inner {
    txs: HashMap<H256, PooledShieldedTx>,
    by_nullifier: HashMap<ShieldedDigest, H256>,
    /// Transactions still in their Dandelion++ stem: held only to relay,
    /// never packed nor counted, so a stem node does not reveal itself as
    /// close to the origin by mining or reporting them (spec §9.6).
    stem: HashSet<H256>,
    /// Transactions seen spent (on chain), with the height first seen: kept
    /// for `HOLD_EPOCHS` so a reorg that undoes their block can repack them.
    held: HashMap<H256, u64>,
}

/// What packing makes of a pooled transaction.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Pack,
    /// Already on chain: kept `HOLD_EPOCHS` in case a reorg undoes it.
    HoldOnChain,
    /// Not packable yet (its deposit is not credited, the circuit breaker
    /// is full): kept `SHORT_HOLD_EPOCHS`, then the wallet resubmits. Short,
    /// because anyone can make such a transaction without funds.
    HoldShort,
    /// Never packable again (its anchor expired).
    Drop,
}

/// How long a transaction on chain is kept: well past any reorg depth.
pub const HOLD_EPOCHS: u64 = 1000;
/// How long a transaction that is not packable yet is kept.
pub const SHORT_HOLD_EPOCHS: u64 = 30;

pub struct ShieldedTxPool {
    inner: RwLock<Inner>,
    capacity: usize,
    /// Transactions submitted to this node, waiting for the network layer
    /// to send them into a Dandelion++ stem.
    originated: parking_lot::Mutex<Vec<H256>>,
}

impl ShieldedTxPool {
    /// Each transaction holds a ~190 KB proof, so the pool is small.
    pub const DEFAULT_CAPACITY: usize = 256;

    pub fn new(capacity: usize) -> Self {
        ShieldedTxPool {
            inner: Default::default(),
            capacity,
            originated: Default::default(),
        }
    }

    /// Marks a transaction as submitted to this node.
    pub fn mark_originated(&self, hash: H256) { self.originated.lock().push(hash); }

    pub fn take_originated(&self) -> Vec<H256> {
        std::mem::take(&mut *self.originated.lock())
    }

    /// Transactions publicly known: fluffed, so packable.
    pub fn len(&self) -> usize {
        let inner = self.inner.read();
        inner.txs.len().saturating_sub(inner.stem.len())
    }

    /// Transactions waiting to be packed: public and not already on chain.
    pub fn pending_len(&self) -> usize {
        let inner = self.inner.read();
        inner.txs.len().saturating_sub(inner.stem.len() + inner.held.len())
    }

    /// Ends the stem phase of `hash`: from now on it may be packed.
    pub fn mark_fluffed(&self, hash: &H256) { self.inner.write().stem.remove(hash); }

    pub fn contains(&self, hash: &H256) -> bool {
        self.inner.read().txs.contains_key(hash)
    }

    pub fn get(&self, hash: &H256) -> Option<PooledShieldedTx> {
        self.inner.read().txs.get(hash).cloned()
    }

    /// The pool-state checks of `insert`, without inserting: run before the
    /// expensive proof verification, so a duplicate or a conflict costs no
    /// STARK (spec §8.2).
    pub fn precheck(&self, body: &ShieldedTxBody) -> Result<(), ShieldedPoolError> {
        let inner = self.inner.read();
        if inner.txs.contains_key(&body.hash()) {
            return Err(ShieldedPoolError::Duplicate);
        }
        if body.nf.iter().any(|nf| inner.by_nullifier.contains_key(nf)) {
            return Err(ShieldedPoolError::Conflict);
        }
        if inner.txs.len() >= self.capacity && inner.held.is_empty() {
            return Err(ShieldedPoolError::Full);
        }
        Ok(())
    }

    /// Adds a transaction that already passed every check that needs no
    /// pool state. The first transaction to spend a nullifier wins. A `stem`
    /// transaction stays out of `pick` until `mark_fluffed`.
    pub fn insert(&self, tx: PooledShieldedTx, stem: bool) -> Result<(), ShieldedPoolError> {
        let mut inner = self.inner.write();
        if inner.txs.contains_key(&tx.hash) {
            return Err(ShieldedPoolError::Duplicate);
        }
        if tx.body.nf.iter().any(|nf| inner.by_nullifier.contains_key(nf)) {
            return Err(ShieldedPoolError::Conflict);
        }
        // Stems, which a node cannot fully check, take at most a quarter of
        // the pool: they cannot crowd out what is packable.
        if stem && inner.stem.len() >= self.capacity / 4 {
            return Err(ShieldedPoolError::Full);
        }
        if inner.txs.len() >= self.capacity {
            // A held transaction (not packable now) gives way to a new one.
            match inner.held.keys().next().copied() {
                Some(evict) => {
                    Self::remove_locked(&mut inner, &evict);
                }
                None => return Err(ShieldedPoolError::Full),
            }
        }
        for nf in &tx.body.nf {
            inner.by_nullifier.insert(*nf, tx.hash);
        }
        if stem {
            inner.stem.insert(tx.hash);
        }
        inner.txs.insert(tx.hash, tx);
        Ok(())
    }

    pub fn remove(&self, hash: &H256) -> Option<PooledShieldedTx> {
        Self::remove_locked(&mut self.inner.write(), hash)
    }

    fn remove_locked(inner: &mut Inner, hash: &H256) -> Option<PooledShieldedTx> {
        let tx = inner.txs.remove(hash)?;
        inner.stem.remove(hash);
        inner.held.remove(hash);
        for nf in &tx.body.nf {
            inner.by_nullifier.remove(nf);
        }
        Some(tx)
    }

    /// Up to `max` transactions in random order (not first-come: a spammer
    /// cannot hold the front of the queue, spec §8.3) that `verdict` at
    /// `height` accepts. `Drop` removes a transaction; `Hold` keeps it
    /// unpacked, at most `HOLD_EPOCHS`.
    pub fn pick(
        &self, max: usize, height: u64,
        mut verdict: impl FnMut(&ShieldedTxBody) -> Verdict,
    ) -> Vec<PooledShieldedTx> {
        let mut candidates: Vec<PooledShieldedTx> = {
            let inner = self.inner.read();
            inner
                .txs
                .values()
                .filter(|tx| !inner.stem.contains(&tx.hash))
                .cloned()
                .collect()
        };
        candidates.shuffle(&mut rand::thread_rng());
        let mut picked = Vec::new();
        let mut nullifiers = HashSet::new();
        for tx in candidates {
            let v = verdict(&tx.body);
            match v {
                Verdict::Drop => {
                    self.remove(&tx.hash);
                    continue;
                }
                Verdict::HoldOnChain | Verdict::HoldShort => {
                    let ttl = if v == Verdict::HoldOnChain { HOLD_EPOCHS } else { SHORT_HOLD_EPOCHS };
                    let mut inner = self.inner.write();
                    // Removed meanwhile: nothing to hold.
                    if !inner.txs.contains_key(&tx.hash) {
                        continue;
                    }
                    // Held transactions take at most half the pool.
                    if !inner.held.contains_key(&tx.hash)
                        && inner.held.len() >= self.capacity / 2
                    {
                        Self::remove_locked(&mut inner, &tx.hash);
                        continue;
                    }
                    let since = *inner.held.entry(tx.hash).or_insert(height);
                    if height.saturating_sub(since) > ttl {
                        Self::remove_locked(&mut inner, &tx.hash);
                    }
                    continue;
                }
                Verdict::Pack => {
                    self.inner.write().held.remove(&tx.hash);
                }
            }
            if picked.len() < max && tx.body.nf.iter().all(|nf| nullifiers.insert(*nf)) {
                picked.push(tx);
            }
        }
        picked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(seed: u32) -> PooledShieldedTx {
        PooledShieldedTx::new(ShieldedTxBody::sample_for_tests(seed), vec![seed as u8])
    }

    #[test]
    fn duplicates_conflicts_and_capacity() {
        let pool = ShieldedTxPool::new(2);
        pool.insert(tx(1), false).unwrap();
        assert_eq!(pool.insert(tx(1), false), Err(ShieldedPoolError::Duplicate));
        let mut conflicting = ShieldedTxBody::sample_for_tests(2);
        conflicting.nf[1] = tx(1).body.nf[0];
        assert_eq!(
            pool.insert(PooledShieldedTx::new(conflicting, vec![]), false),
            Err(ShieldedPoolError::Conflict)
        );
        pool.insert(tx(3), false).unwrap();
        assert_eq!(pool.insert(tx(4), false), Err(ShieldedPoolError::Full));
        assert!(pool.remove(&tx(1).hash).is_some());
        pool.insert(tx(4), false).unwrap();
    }

    #[test]
    fn pick_respects_the_limit_and_drops_invalid() {
        let pool = ShieldedTxPool::new(10);
        for i in 0..6 {
            pool.insert(tx(i), false).unwrap();
        }
        assert_eq!(pool.pick(4, 0, |_| Verdict::Pack).len(), 4);
        let bad = tx(2).body.hash();
        let picked = pool.pick(10, 0, |b| if b.hash() == bad { Verdict::Drop } else { Verdict::Pack });
        assert_eq!(picked.len(), 5);
        assert!(!pool.contains(&bad), "invalid transactions leave the pool");
    }

    #[test]
    fn stem_transactions_are_neither_packed_nor_counted() {
        let pool = ShieldedTxPool::new(10);
        pool.insert(tx(1), true).unwrap();
        pool.insert(tx(2), false).unwrap();
        assert_eq!(pool.len(), 1);
        assert!(pool.contains(&tx(1).hash), "still held for relaying");
        let picked = pool.pick(10, 0, |_| Verdict::Pack);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].hash, tx(2).hash);
        pool.mark_fluffed(&tx(1).hash);
        assert_eq!(pool.len(), 2);
        assert_eq!(pool.pick(10, 0, |_| Verdict::Pack).len(), 2);
    }

    #[test]
    fn spent_transactions_are_held_through_reorgs_then_evicted() {
        let pool = ShieldedTxPool::new(10);
        pool.insert(tx(1), false).unwrap();
        // Seen on chain at height 100: held, not packed.
        assert!(pool.pick(10, 100, |_| Verdict::HoldOnChain).is_empty());
        // A reorg undid the block: packable again.
        assert_eq!(pool.pick(10, 110, |_| Verdict::Pack).len(), 1);
        // On chain again, for good.
        pool.pick(10, 200, |_| Verdict::HoldOnChain);
        pool.pick(10, 200 + HOLD_EPOCHS, |_| Verdict::HoldOnChain);
        assert!(pool.contains(&tx(1).hash), "held up to HOLD_EPOCHS");
        assert_eq!(pool.pending_len(), 0, "a held transaction is not pending");
        pool.pick(10, 201 + HOLD_EPOCHS, |_| Verdict::HoldOnChain);
        assert!(!pool.contains(&tx(1).hash), "then evicted");
    }

    #[test]
    fn unpackable_transactions_are_held_briefly_and_give_way() {
        let pool = ShieldedTxPool::new(4);
        for i in 0..4 {
            pool.insert(tx(i), false).unwrap();
        }
        // No deposit yet: held for SHORT_HOLD_EPOCHS only.
        pool.pick(10, 100, |b| if b.hash() == tx(0).hash { Verdict::HoldShort } else { Verdict::Pack });
        pool.pick(10, 100 + SHORT_HOLD_EPOCHS + 1, |b| if b.hash() == tx(0).hash { Verdict::HoldShort } else { Verdict::Pack });
        assert!(!pool.contains(&tx(0).hash), "evicted after the short hold");
        // A full pool makes room for a new transaction by evicting a held one.
        pool.insert(tx(4), false).unwrap();
        pool.pick(10, 200, |b| if b.hash() == tx(1).hash { Verdict::HoldOnChain } else { Verdict::Pack });
        pool.insert(tx(5), false).unwrap();
        assert!(!pool.contains(&tx(1).hash), "the held one gave way");
        assert!(pool.contains(&tx(5).hash));
    }

    #[test]
    fn stems_take_at_most_a_quarter_of_the_pool() {
        let pool = ShieldedTxPool::new(8);
        pool.insert(tx(1), true).unwrap();
        pool.insert(tx(2), true).unwrap();
        assert_eq!(pool.insert(tx(3), true), Err(ShieldedPoolError::Full));
        pool.insert(tx(3), false).unwrap();
    }
}
