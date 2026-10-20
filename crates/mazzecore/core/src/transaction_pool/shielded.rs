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
}

#[derive(Default)]
struct Inner {
    txs: HashMap<H256, PooledShieldedTx>,
    by_nullifier: HashMap<ShieldedDigest, H256>,
}

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

    pub fn len(&self) -> usize { self.inner.read().txs.len() }

    pub fn contains(&self, hash: &H256) -> bool {
        self.inner.read().txs.contains_key(hash)
    }

    pub fn get(&self, hash: &H256) -> Option<PooledShieldedTx> {
        self.inner.read().txs.get(hash).cloned()
    }

    /// Adds a transaction that already passed every check that needs no
    /// pool state. The first transaction to spend a nullifier wins.
    pub fn insert(&self, tx: PooledShieldedTx) -> Result<(), ShieldedPoolError> {
        let mut inner = self.inner.write();
        if inner.txs.contains_key(&tx.hash) {
            return Err(ShieldedPoolError::Duplicate);
        }
        if tx.body.nf.iter().any(|nf| inner.by_nullifier.contains_key(nf)) {
            return Err(ShieldedPoolError::Conflict);
        }
        if inner.txs.len() >= self.capacity {
            return Err(ShieldedPoolError::Full);
        }
        for nf in &tx.body.nf {
            inner.by_nullifier.insert(*nf, tx.hash);
        }
        inner.txs.insert(tx.hash, tx);
        Ok(())
    }

    pub fn remove(&self, hash: &H256) -> Option<PooledShieldedTx> {
        let mut inner = self.inner.write();
        let tx = inner.txs.remove(hash)?;
        for nf in &tx.body.nf {
            inner.by_nullifier.remove(nf);
        }
        Some(tx)
    }

    /// Up to `max` transactions in random order (not first-come: a spammer
    /// cannot hold the front of the queue, spec §8.3), keeping only those
    /// `still_valid` accepts and dropping the others from the pool.
    pub fn pick(
        &self, max: usize, mut still_valid: impl FnMut(&ShieldedTxBody) -> bool,
    ) -> Vec<PooledShieldedTx> {
        let mut candidates: Vec<PooledShieldedTx> =
            self.inner.read().txs.values().cloned().collect();
        candidates.shuffle(&mut rand::thread_rng());
        let mut picked = Vec::new();
        let mut nullifiers = HashSet::new();
        for tx in candidates {
            if picked.len() >= max {
                break;
            }
            if !still_valid(&tx.body) {
                self.remove(&tx.hash);
                continue;
            }
            if tx.body.nf.iter().all(|nf| nullifiers.insert(*nf)) {
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
        pool.insert(tx(1)).unwrap();
        assert_eq!(pool.insert(tx(1)), Err(ShieldedPoolError::Duplicate));
        let mut conflicting = ShieldedTxBody::sample_for_tests(2);
        conflicting.nf[1] = tx(1).body.nf[0];
        assert_eq!(
            pool.insert(PooledShieldedTx::new(conflicting, vec![])),
            Err(ShieldedPoolError::Conflict)
        );
        pool.insert(tx(3)).unwrap();
        assert_eq!(pool.insert(tx(4)), Err(ShieldedPoolError::Full));
        assert!(pool.remove(&tx(1).hash).is_some());
        pool.insert(tx(4)).unwrap();
    }

    #[test]
    fn pick_respects_the_limit_and_drops_invalid() {
        let pool = ShieldedTxPool::new(10);
        for i in 0..6 {
            pool.insert(tx(i)).unwrap();
        }
        assert_eq!(pool.pick(4, |_| true).len(), 4);
        let bad = tx(2).body.hash();
        let picked = pool.pick(10, |b| b.hash() != bad);
        assert_eq!(picked.len(), 5);
        assert!(!pool.contains(&bad), "invalid transactions leave the pool");
    }
}
