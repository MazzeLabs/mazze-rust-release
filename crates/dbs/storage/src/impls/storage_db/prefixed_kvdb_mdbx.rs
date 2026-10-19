// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Per-snapshot delta MPT storage inside a shared MDBX column
//! (Storage Phase 4c).
//!
//! Every delta MPT node key is prefixed with the snapshot's
//! 32-byte `EpochId`. All snapshots share the same MDBX column
//! ([`MdbxColumn::DeltaMpt`](super::mdbx_columns::Column::DeltaMpt)),
//! so different snapshots' node sets sort contiguously in the
//! B+tree and never collide. Destroy is a single cursor-driven
//! range delete on `[prefix, prefix + 1)`.
//!
//! # Why prefix, not per-snapshot MDBX env
//!
//! - Opening one env per snapshot means one file per snapshot →
//!   the paritydb-log-file thrash we already saw on archives at
//!   restart (100k+ open fds).
//! - MDBX caps sub-tables per env at `set_max_tables(64)` — one
//!   sub-table per snapshot would blow through this quickly on
//!   archive nodes retaining hundreds of snapshots.
//! - MDBX B+tree ordering makes prefix isolation trivial: a
//!   32-byte epoch_id prefix gives every snapshot a contiguous
//!   slice with no need for extra separator bytes.
//!
//! # Migration
//!
//! See
//! [`docs/internal/storage-delta-mpt-migration.md`](../../../../../docs/internal/storage-delta-mpt-migration.md).
//! Coexistence with the paritydb backend is deliberately NOT
//! supported (per Q6.1 of the design doc): MDBX-native from day
//! one, wipe-and-relaunch to cut over.

use super::kvdb_mdbx::{BatchOp, KvdbMdbx, KvdbMdbxTransaction};
use crate::{
    impls::errors::*,
    storage_db::{delta_db_manager::DeltaDbTrait, key_value_db::*},
};
use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};
use metrics::{Counter, CounterUsize};
use std::{any::Any, sync::Arc};

/// Per-snapshot counters bumped on each read/write against a
/// [`PrefixedKvdbMdbx`]. Registered under
/// `mdbx_delta_mpt.<hex(epoch_id)>` so operator dashboards get
/// per-snapshot visibility instead of one aggregated number. Cheap
/// `Arc<dyn Counter>` clones live in the wrapping
/// `PrefixedKvdbMdbx`; a `None` metrics field means the caller
/// opted out (tests, or a path where the manager doesn't own the
/// snapshot yet).
pub struct DeltaMptSnapshotMetrics {
    pub puts_ok: Arc<dyn Counter<usize>>,
    pub puts_fail: Arc<dyn Counter<usize>>,
    pub gets_ok: Arc<dyn Counter<usize>>,
    pub gets_miss: Arc<dyn Counter<usize>>,
    pub deletes_ok: Arc<dyn Counter<usize>>,
    pub deletes_fail: Arc<dyn Counter<usize>>,
}

impl DeltaMptSnapshotMetrics {
    /// Register a fresh set of counters keyed by a group name —
    /// typically `"mdbx_delta_mpt." + hex(epoch_id)`. Called once
    /// per snapshot at construction; the returned bundle stays
    /// alive for the snapshot's lifetime.
    pub fn register(group: &str) -> Self {
        Self {
            puts_ok: CounterUsize::register_with_group(group, "puts_ok"),
            puts_fail: CounterUsize::register_with_group(
                group,
                "puts_fail",
            ),
            gets_ok: CounterUsize::register_with_group(group, "gets_ok"),
            gets_miss: CounterUsize::register_with_group(
                group,
                "gets_miss",
            ),
            deletes_ok: CounterUsize::register_with_group(
                group,
                "deletes_ok",
            ),
            deletes_fail: CounterUsize::register_with_group(
                group,
                "deletes_fail",
            ),
        }
    }
}

/// The 4c delta-MPT snapshot prefix length. Kept as a public const
/// for delta-MPT callers that want to assert their `EpochId`
/// serialises to the expected width; the primitive itself accepts
/// any prefix length so the same wrapper can back 5c's 33-byte
/// (32B epoch id + 1B sub-prefix) snapshot chunk store as well.
pub const DELTA_MPT_PREFIX_LEN: usize = 32;

/// A view into one snapshot's slice of a shared MDBX column. Cheap
/// to `Clone` — shares the underlying `KvdbMdbx` (which itself
/// shares the `Arc<MdbxEnv>`).
///
/// **Invariant**: every read / write / delete transparently
/// prepends `prefix` before hitting MDBX; iteration is bounded to
/// `[prefix, prefix + 1)` so it never leaks into an adjacent
/// snapshot's slice.
///
/// **Prefix length is runtime, not type-level** — 4c uses 32 bytes
/// (bare `EpochId`), 5c uses 33 bytes (`EpochId | sub_prefix`).
/// The design doc's §2.3.2 fallback ("`Vec<u8>` prefix is
/// acceptable if the const-generic ripple is noisy") is taken:
/// there's no hot-path benefit to `PrefixedKvdbMdbx<const N>` — the
/// prefix bytes are pointer-behind-Arc regardless, and the
/// composed-key `Vec` allocation dominates.
pub struct PrefixedKvdbMdbx {
    inner: KvdbMdbx,
    /// Per-snapshot prefix, shared via `Arc` so clones don't
    /// reallocate. 32B for delta MPTs, 33B for snapshot chunks.
    prefix: Arc<Vec<u8>>,
    /// Optional per-snapshot metrics. `None` for tests and for
    /// short-lived handles the manager hasn't registered with the
    /// metric group. When `Some`, every put/get/delete bumps the
    /// appropriate counter — no aggregation, per-snapshot
    /// visibility per Phase 4c design §6.5.
    metrics: Option<Arc<DeltaMptSnapshotMetrics>>,
}

impl Clone for PrefixedKvdbMdbx {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            prefix: Arc::clone(&self.prefix),
            metrics: self.metrics.clone(),
        }
    }
}

impl MallocSizeOf for PrefixedKvdbMdbx {
    fn size_of(&self, _ops: &mut MallocSizeOfOps) -> usize {
        // Prefix is a small fixed allocation shared via Arc; inner
        // is 0-sized per KvdbMdbx impl. Total ~= 0.
        0
    }
}

impl PrefixedKvdbMdbx {
    /// Wrap an MDBX column handle with a per-snapshot prefix. No
    /// metrics registered — used by tests and by callers that
    /// don't need per-snapshot dashboards.
    ///
    /// Panics if `prefix` is empty — an empty prefix would make
    /// [`Self::upper_bound_exclusive`] unable to bound the scan and
    /// would let iteration leak into other snapshots' slices.
    pub fn new(inner: KvdbMdbx, prefix: Vec<u8>) -> Self {
        assert!(
            !prefix.is_empty(),
            "PrefixedKvdbMdbx::new: prefix must not be empty"
        );
        Self {
            inner,
            prefix: Arc::new(prefix),
            metrics: None,
        }
    }

    /// Same as [`Self::new`] but with a per-snapshot metrics
    /// bundle attached. The `DeltaDbManagerMdbx` uses this form so
    /// each snapshot gets its own counter group.
    pub fn new_metered(
        inner: KvdbMdbx, prefix: Vec<u8>,
        metrics: Arc<DeltaMptSnapshotMetrics>,
    ) -> Self {
        assert!(
            !prefix.is_empty(),
            "PrefixedKvdbMdbx::new_metered: prefix must not be empty"
        );
        Self {
            inner,
            prefix: Arc::new(prefix),
            metrics: Some(metrics),
        }
    }

    /// Borrowed view of the raw prefix bytes. Used by the manager
    /// for destroy iteration and metrics labels.
    pub fn prefix(&self) -> &[u8] {
        &self.prefix
    }

    /// Concatenate `prefix ++ key` into a fresh buffer. Kept as a
    /// small inlineable helper because every op on the hot path
    /// funnels through it.
    #[inline]
    fn compose(&self, key: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.prefix.len() + key.len());
        out.extend_from_slice(&self.prefix[..]);
        out.extend_from_slice(key);
        out
    }

    /// The exclusive upper bound for prefix-scoped scans: the
    /// smallest key strictly greater than every possible
    /// `prefix ++ k`. Computed by incrementing the prefix
    /// numerically, with a `None` result reserved for the
    /// (astronomically improbable) case where the prefix is
    /// `0xff…ff` — then there's no upper bound because everything
    /// with a higher prefix is by definition outside this slice.
    ///
    /// Consumed by [`Self::destroy_by_prefix`] and by future range
    /// scans (`iter_range`) that need to stay inside the snapshot.
    pub fn upper_bound_exclusive(&self) -> Option<Vec<u8>> {
        let mut ub: Vec<u8> = (*self.prefix).clone();
        for byte in ub.iter_mut().rev() {
            if *byte == 0xff {
                *byte = 0;
            } else {
                *byte += 1;
                return Some(ub);
            }
        }
        None
    }

    /// The underlying `KvdbMdbx` handle. Escape hatch for the
    /// manager's destroy path which needs to issue a range delete
    /// across the whole prefix. Not exposed publicly — callers
    /// should go through the trait surface for anything else.
    pub(super) fn inner_kvdb(&self) -> &KvdbMdbx {
        &self.inner
    }
}

// -------------------------- read path --------------------------

impl KeyValueDbTypes for PrefixedKvdbMdbx {
    type ValueType = Box<[u8]>;
}

impl KeyValueDbTraitRead for PrefixedKvdbMdbx {
    fn get(&self, key: &[u8]) -> Result<Option<Box<[u8]>>> {
        let out = self.inner.get(&self.compose(key))?;
        if let Some(m) = &self.metrics {
            if out.is_some() {
                m.gets_ok.inc(1);
            } else {
                m.gets_miss.inc(1);
            }
        }
        Ok(out)
    }
}

// MDBX gives us concurrent readers via MVCC; register that fact so
// `KeyValueDbToOwnedReadTrait` picks up the blanket impl for
// multi-reader backends. Mirrors `mark_kvdb_multi_reader!(KvdbMdbx)`
// in `kvdb_mdbx.rs`.
mark_kvdb_multi_reader!(PrefixedKvdbMdbx);

// Explicit `KeyValueDbTraitOwnedRead` impl on the plain type —
// `mark_kvdb_multi_reader!` only supplies the `&PrefixedKvdbMdbx`
// blanket, and Phase 5c's snapshot MPT machinery
// (`SnapshotMpt<PrefixedKvdbMdbx, PrefixedKvdbMdbx>`,
// `SnapshotMptTraitReadAndIterate`) hits generic bounds that
// resolve against the plain type. Same read behaviour as
// `KeyValueDbTraitRead::get` — MDBX reads are lock-free MVCC so
// there's no semantic difference between the shared-borrow and
// owned-read paths.
impl KeyValueDbTraitOwnedRead for PrefixedKvdbMdbx {
    fn get_mut(&mut self, key: &[u8]) -> Result<Option<Box<[u8]>>> {
        <Self as KeyValueDbTraitRead>::get(&*self, key)
    }
}

// -------------------------- write path --------------------------

impl KeyValueDbTrait for PrefixedKvdbMdbx {
    fn delete(&self, key: &[u8]) -> Result<Option<Option<Box<[u8]>>>> {
        match self.inner.delete(&self.compose(key)) {
            Ok(v) => {
                if let Some(m) = &self.metrics {
                    m.deletes_ok.inc(1);
                }
                Ok(v)
            }
            Err(e) => {
                if let Some(m) = &self.metrics {
                    m.deletes_fail.inc(1);
                }
                Err(e)
            }
        }
    }

    fn put(
        &self, key: &[u8], value: &[u8],
    ) -> Result<Option<Option<Box<[u8]>>>> {
        Self::put_impl(self, key, value)
    }
}

impl PrefixedKvdbMdbx {
    #[inline]
    fn put_impl(
        &self, key: &[u8], value: &[u8],
    ) -> Result<Option<Option<Box<[u8]>>>> {
        match self.inner.put(&self.compose(key), value) {
            Ok(v) => {
                if let Some(m) = &self.metrics {
                    m.puts_ok.inc(1);
                }
                Ok(v)
            }
            Err(e) => {
                if let Some(m) = &self.metrics {
                    m.puts_fail.inc(1);
                }
                Err(e)
            }
        }
    }
}

// `KeyValueDbTraitSingleWriter` — required by Phase 5c's
// `SnapshotMptTraitRw for SnapshotMpt<PrefixedKvdbMdbx,
// PrefixedKvdbMdbx>` bound (see the trait's `KeyValueDbTraitSingleWriter<ValueType = SnapshotMptDbValue>`
// where-clause). Delegates to the shared-borrow `KeyValueDbTrait`
// variants above — MDBX is single-writer at the env level, so the
// `&mut self` promise is respected via the underlying rw_txn
// anyway.
impl KeyValueDbTraitSingleWriter for PrefixedKvdbMdbx {
    fn delete(
        &mut self, key: &[u8],
    ) -> Result<Option<Option<Box<[u8]>>>> {
        <Self as KeyValueDbTrait>::delete(&*self, key)
    }

    fn put(
        &mut self, key: &[u8], value: &[u8],
    ) -> Result<Option<Option<Box<[u8]>>>> {
        Self::put_impl(&*self, key, value)
    }
}

// -------------------------- batch write --------------------------

impl PrefixedKvdbMdbx {
    /// Batched write with the same atomicity contract as
    /// [`KvdbMdbx::write_batch`]. Every op has its key prefixed
    /// before it lands in the shared MDBX rw_txn.
    #[allow(dead_code)]
    pub fn write_batch(&self, ops: &[BatchOp]) -> Result<()> {
        // Compose each key with the prefix once, then hand the
        // owned buffers to KvdbMdbx::write_batch via BatchOp
        // borrows.
        let composed: Vec<(Vec<u8>, Option<&[u8]>)> = ops
            .iter()
            .map(|op| match op {
                BatchOp::Put(k, v) => (self.compose(k), Some(*v)),
                BatchOp::Delete(k) => (self.compose(k), None),
            })
            .collect();
        let borrowed: Vec<BatchOp> = composed
            .iter()
            .map(|(k, v)| match v {
                Some(val) => BatchOp::Put(k.as_slice(), *val),
                None => BatchOp::Delete(k.as_slice()),
            })
            .collect();
        self.inner.write_batch(&borrowed)
    }
}

// -------------------------- transactional path --------------------------

/// Buffered writes against one snapshot. Every op gets its key
/// prefixed before pushing to the inner MDBX transaction, so
/// commit() ends up doing exactly the same rw_txn work
/// [`KvdbMdbxTransaction::commit`] does.
pub struct PrefixedKvdbMdbxTransaction {
    inner: KvdbMdbxTransaction,
    prefix: Arc<Vec<u8>>,
}

impl PrefixedKvdbMdbxTransaction {
    #[inline]
    fn compose(&self, key: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.prefix.len() + key.len());
        out.extend_from_slice(&self.prefix[..]);
        out.extend_from_slice(key);
        out
    }
}

impl KeyValueDbTypes for PrefixedKvdbMdbxTransaction {
    type ValueType = Box<[u8]>;
}

impl KeyValueDbTraitOwnedRead for PrefixedKvdbMdbxTransaction {
    fn get_mut(&mut self, _key: &[u8]) -> Result<Option<Box<[u8]>>> {
        // Matches KvdbMdbxTransaction: writes-only buffer, no
        // in-transaction reads. Callers read through the parent
        // PrefixedKvdbMdbx.
        unreachable!(
            "PrefixedKvdbMdbxTransaction does not support reads — \
             use the parent PrefixedKvdbMdbx::get"
        )
    }
}

impl KeyValueDbTraitSingleWriter for PrefixedKvdbMdbxTransaction {
    fn delete(&mut self, key: &[u8]) -> Result<Option<Option<Box<[u8]>>>> {
        let composed = self.compose(key);
        self.inner.delete(&composed)
    }

    fn put(
        &mut self, key: &[u8], value: &[u8],
    ) -> Result<Option<Option<Box<[u8]>>>> {
        let composed = self.compose(key);
        self.inner.put(&composed, value)
    }
}

impl KeyValueDbTransactionTrait for PrefixedKvdbMdbxTransaction {
    fn commit(&mut self, db: &dyn Any) -> Result<()> {
        // The wrapper takes a `PrefixedKvdbMdbx`, but the inner txn
        // wants its own `KvdbMdbx`. Downcast, verify the prefix
        // matches, then delegate.
        let prefixed = match db.downcast_ref::<PrefixedKvdbMdbx>() {
            Some(p) => p,
            None => bail!(
                "PrefixedKvdbMdbxTransaction::commit: db is not a \
                 PrefixedKvdbMdbx instance"
            ),
        };
        if prefixed.prefix != self.prefix {
            bail!(
                "PrefixedKvdbMdbxTransaction::commit: prefix \
                 mismatch — refusing to commit into a different \
                 snapshot's slice"
            );
        }
        self.inner.commit(prefixed.inner_kvdb() as &dyn Any)
    }

    fn revert(&mut self) -> Result<()> {
        self.inner.revert()
    }

    fn restart(
        &mut self, immediate_write: bool, no_revert: bool,
    ) -> Result<()> {
        self.inner.restart(immediate_write, no_revert)
    }
}

impl KeyValueDbTraitTransactional for PrefixedKvdbMdbx {
    type TransactionType = PrefixedKvdbMdbxTransaction;

    fn start_transaction(
        &self, immediate_write: bool,
    ) -> Result<Self::TransactionType> {
        Ok(PrefixedKvdbMdbxTransaction {
            inner: self.inner.start_transaction(immediate_write)?,
            prefix: Arc::clone(&self.prefix),
        })
    }
}

impl Drop for PrefixedKvdbMdbxTransaction {
    fn drop(&mut self) {
        // Pending ops on the inner txn drop silently — matches
        // KvdbMdbxTransaction and KvdbParityDbTransaction.
    }
}

// -------------------------- marker --------------------------

impl DeltaDbTrait for PrefixedKvdbMdbx {}

// -------------------------- tests --------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impls::storage_db::{
        kvdb_mdbx::MdbxEnv, mdbx_columns::Column,
    };
    use tempdir::TempDir;

    fn open_env() -> (TempDir, Arc<MdbxEnv>) {
        let dir = TempDir::new("prefixed_kvdb_mdbx").unwrap();
        let env = MdbxEnv::open(dir.path()).unwrap();
        (dir, env)
    }

    fn make_prefixed(prefix: u8) -> (TempDir, PrefixedKvdbMdbx) {
        let (dir, env) = open_env();
        let kvdb = KvdbMdbx::with_column(env, Column::DeltaMpt.id());
        (dir, PrefixedKvdbMdbx::new(kvdb, vec![prefix; DELTA_MPT_PREFIX_LEN]))
    }

    /// Round-trip put/get/delete without touching the prefix. The
    /// caller-visible keys look exactly like the underlying MDBX
    /// column — that's what the delta-MPT code depends on.
    #[test]
    fn put_get_delete_round_trip() {
        let (_dir, p) = make_prefixed(0x11);
        assert!(p.get(b"missing").unwrap().is_none());
        p.put(b"k", b"v").unwrap();
        assert_eq!(p.get(b"k").unwrap().as_deref(), Some(&b"v"[..]));
        p.delete(b"k").unwrap();
        assert!(p.get(b"k").unwrap().is_none());
    }

    /// Two prefixes on the SAME underlying column don't see each
    /// other's keys. This is the core isolation guarantee.
    #[test]
    fn distinct_prefixes_isolated() {
        let (_dir, env) = open_env();
        let col = Column::DeltaMpt.id();
        let a = PrefixedKvdbMdbx::new(
            KvdbMdbx::with_column(Arc::clone(&env), col),
            vec![0xaa; DELTA_MPT_PREFIX_LEN],
        );
        let b = PrefixedKvdbMdbx::new(
            KvdbMdbx::with_column(env, col),
            vec![0xbb; DELTA_MPT_PREFIX_LEN],
        );
        a.put(b"key", b"from-a").unwrap();
        b.put(b"key", b"from-b").unwrap();
        assert_eq!(a.get(b"key").unwrap().as_deref(), Some(&b"from-a"[..]));
        assert_eq!(b.get(b"key").unwrap().as_deref(), Some(&b"from-b"[..]));
    }

    /// Transactional commit lands the whole batch atomically under
    /// the prefix — no leak to a neighbouring snapshot's slice.
    #[test]
    fn transactional_commit_stays_scoped() {
        let (_dir, env) = open_env();
        let col = Column::DeltaMpt.id();
        let target = PrefixedKvdbMdbx::new(
            KvdbMdbx::with_column(Arc::clone(&env), col),
            vec![0x01; DELTA_MPT_PREFIX_LEN],
        );
        let neighbour = PrefixedKvdbMdbx::new(
            KvdbMdbx::with_column(env, col),
            vec![0x02; DELTA_MPT_PREFIX_LEN],
        );

        let mut txn = target.start_transaction(false).unwrap();
        txn.put(b"a", b"1").unwrap();
        txn.put(b"b", b"2").unwrap();
        txn.commit(&target as &dyn Any).unwrap();

        assert_eq!(target.get(b"a").unwrap().as_deref(), Some(&b"1"[..]));
        assert_eq!(target.get(b"b").unwrap().as_deref(), Some(&b"2"[..]));
        // Neighbour's slice never saw these keys.
        assert!(neighbour.get(b"a").unwrap().is_none());
        assert!(neighbour.get(b"b").unwrap().is_none());
    }

    /// Committing a transaction against the wrong prefix errors —
    /// prevents a subtle bug where a snapshot's txn accidentally
    /// commits into a neighbour's slice.
    #[test]
    fn commit_rejects_prefix_mismatch() {
        let (_dir, env) = open_env();
        let col = Column::DeltaMpt.id();
        let a = PrefixedKvdbMdbx::new(
            KvdbMdbx::with_column(Arc::clone(&env), col),
            vec![0x03; DELTA_MPT_PREFIX_LEN],
        );
        let b = PrefixedKvdbMdbx::new(
            KvdbMdbx::with_column(env, col),
            vec![0x04; DELTA_MPT_PREFIX_LEN],
        );
        let mut txn = a.start_transaction(false).unwrap();
        txn.put(b"k", b"v").unwrap();
        // Committing against `b` (wrong prefix) must fail loudly.
        assert!(txn.commit(&b as &dyn Any).is_err());
    }

    /// `upper_bound_exclusive` returns the numerically-next 32-byte
    /// value for a normal prefix, and `None` when the prefix is
    /// `0xff…ff` (there is no representable next value).
    #[test]
    fn upper_bound_shape() {
        let (_dir, p) = make_prefixed(0x00);
        assert_eq!(p.prefix(), &[0x00; DELTA_MPT_PREFIX_LEN][..]);
        let mut expected = vec![0x00; DELTA_MPT_PREFIX_LEN];
        expected[DELTA_MPT_PREFIX_LEN - 1] = 0x01;
        assert_eq!(p.upper_bound_exclusive(), Some(expected));

        let (_dir_hi, hi) = make_prefixed(0xff);
        assert_eq!(hi.upper_bound_exclusive(), None);
    }

    /// A 33-byte prefix works end-to-end: puts, gets, upper bound
    /// composition, and isolation. This exercises the design goal
    /// of hosting Phase 5c's `[EpochId | sub_prefix]` layout on the
    /// same primitive.
    #[test]
    fn variable_length_prefix_end_to_end() {
        let (_dir, env) = open_env();
        let col = Column::DeltaMpt.id();
        let mut prefix_33 = vec![0x55u8; 33];
        prefix_33[32] = 0xa1;
        let p = PrefixedKvdbMdbx::new(
            KvdbMdbx::with_column(Arc::clone(&env), col),
            prefix_33.clone(),
        );
        p.put(b"k", b"v").unwrap();
        assert_eq!(p.get(b"k").unwrap().as_deref(), Some(&b"v"[..]));
        assert_eq!(p.prefix(), prefix_33.as_slice());
        // Upper bound is `prefix + 1` — last byte 0xa1 → 0xa2.
        let mut expected = prefix_33.clone();
        expected[32] = 0xa2;
        assert_eq!(p.upper_bound_exclusive(), Some(expected));

        // A neighbouring 33-byte prefix (same first 32B, different
        // sub-byte) does not observe our key.
        let mut neighbour_prefix = vec![0x55u8; 33];
        neighbour_prefix[32] = 0xa2;
        let neighbour = PrefixedKvdbMdbx::new(
            KvdbMdbx::with_column(env, col),
            neighbour_prefix,
        );
        assert!(neighbour.get(b"k").unwrap().is_none());
    }

    /// Empty prefix is rejected — see safety note on `new`.
    #[test]
    #[should_panic(expected = "prefix must not be empty")]
    fn empty_prefix_panics() {
        let (_dir, env) = open_env();
        let kvdb = KvdbMdbx::with_column(env, Column::DeltaMpt.id());
        let _ = PrefixedKvdbMdbx::new(kvdb, Vec::new());
    }

    /// `write_batch` composes prefixes on every op and delegates to
    /// the underlying batched MDBX rw_txn. Mixed puts + deletes.
    #[test]
    fn write_batch_composes_prefixes() {
        let (_dir, p) = make_prefixed(0x77);
        p.put(b"pre", b"already").unwrap();
        let ops = [
            BatchOp::Put(b"a", b"1"),
            BatchOp::Put(b"b", b"2"),
            BatchOp::Delete(b"pre"),
        ];
        p.write_batch(&ops).unwrap();
        assert_eq!(p.get(b"a").unwrap().as_deref(), Some(&b"1"[..]));
        assert_eq!(p.get(b"b").unwrap().as_deref(), Some(&b"2"[..]));
        assert!(p.get(b"pre").unwrap().is_none());
    }
}
