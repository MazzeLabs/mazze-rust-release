// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Memory-mapped MDBX backend for Mazze's *hot tier* state DB.
//!
//! See [`docs/storage-architecture.md`](../../../../../docs/storage-architecture.md)
//! for the two-tier MDBX (hot) + ParityDB (cold) design.
//!
//! ## Trait surface
//!
//! `KvdbMdbx` mirrors [`KvdbParitydb`](super::kvdb_paritydb::KvdbParitydb)
//! one-for-one — same `KeyValueDb*` trait family, same multi-column
//! contract — so consumers swap backends without code changes.
//!
//! ## Concurrency
//!
//! MDBX is single-writer / many-reader. Reads use `begin_ro_txn`
//! (concurrent, MVCC snapshots — no blocking). Writes go through
//! `begin_rw_txn` which serializes globally; this is fine for the
//! hot-tier write rate (per-tx state diffs) and revm's read-heavy
//! mix benefits from the unblocked read path.
//!
//! ## Columns
//!
//! Multi-column support is implemented via MDBX *named tables*. Each
//! column `u32` maps to a sub-table named `col-<id>` which is created
//! on first write. Consumers select a column at `KvdbMdbx`
//! construction time.

use super::super::{
    super::storage_db::{
        delta_db_manager::DeltaDbTrait,
        key_value_db::*,
    },
    errors::*,
};
use error_chain::bail;
use lazy_static::lazy_static;
use libmdbx::{Database, NoWriteMap, TableFlags, WriteFlags};
use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};
use metrics::{
    register_meter_with_group, Counter, CounterUsize, Meter, MeterTimer,
};
use std::{
    any::Any,
    path::Path,
    sync::{Arc, Mutex},
};

// ---- Metrics ---------------------------------------------------------
//
// Six exported gauges/counters for the MDBX hot-tier backend. All under
// the `storage` group so operators see them alongside snapshot / delta
// MPT metrics. Post Phase 1 → Phase 2 wiring, these are the numbers
// that tell us whether the executor is actually pushing writes through
// the batched path (`batch_commit_timer` should dominate,
// `put_calls_total` should stay near zero on a healthy hot loop).
lazy_static! {
    /// Latency histogram of a single `write_batch` commit. Records
    /// per-call wall time so we can spot pathological rw_txn stalls
    /// (MDBX serializes rw_txns globally — long tail here means
    /// contention with another writer or a fsync backup).
    static ref MDBX_BATCH_COMMIT_TIMER: Arc<dyn Meter> =
        register_meter_with_group(
            "timer",
            "storage::mdbx::batch_commit",
        );
    /// Number of ops (put + delete) submitted to `write_batch` — sum
    /// gives total mutations routed through the batched path.
    static ref MDBX_BATCH_OPS_TOTAL: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "storage",
            "mdbx::batch_ops_total",
        );
    /// Latency of an `iter_range_owned` call (open txn + open table +
    /// cursor walk + materialize). Tail here tells us when a caller is
    /// materializing an over-large range and should be pushed toward
    /// the streaming variant when it lands in Phase 2.
    static ref MDBX_RANGE_SCAN_TIMER: Arc<dyn Meter> =
        register_meter_with_group(
            "timer",
            "storage::mdbx::range_scan",
        );
    /// Number of rows returned by `iter_range_owned` cumulatively.
    /// Combined with the timer this gives us rows/sec throughput.
    static ref MDBX_RANGE_SCAN_ROWS: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "storage",
            "mdbx::range_scan_rows",
        );
    /// Individual `put` calls — the unbatched slow path. On a healthy
    /// steady-state executor this should stay near zero; a rising
    /// counter is the signal that some code path is going through the
    /// per-op txn instead of `write_batch`.
    static ref MDBX_PUT_CALLS: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "storage",
            "mdbx::put_calls_total",
        );
    /// Individual `get` calls. Read-path activity is the dominant
    /// signal for MDBX — the hot tier's win is fast, lock-free reads.
    static ref MDBX_GET_CALLS: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "storage",
            "mdbx::get_calls_total",
        );
}

/// Map size used when the caller does not specify one. Derived from
/// [`crate::impls::defaults::DEFAULT_MDBX_MAP_SIZE_MB`] (64 GB by default).
const DEFAULT_MAP_SIZE_BYTES: usize =
    (crate::impls::defaults::DEFAULT_MDBX_MAP_SIZE_MB as usize)
        * 1024
        * 1024;

/// Max number of named tables (columns) per environment. Mazze's
/// ledger DB tops out at `NUM_COLUMNS = 7`; we leave headroom for
/// MDBX-only sub-tables (`accounts`, `storage`, `code`,
/// `recent_topology`) plus future expansion.
const DEFAULT_MAX_TABLES: usize = 64;

/// One MDBX environment, shared via `Arc` across all column handles.
pub struct MdbxEnv {
    db: Database<NoWriteMap>,
}

impl MdbxEnv {
    /// Open (or create) an MDBX environment at the given path.
    pub fn open(path: &Path) -> Result<Arc<Self>> {
        Self::open_with_map_size(path, DEFAULT_MAP_SIZE_BYTES)
    }

    pub fn open_with_map_size(
        path: &Path, map_size_bytes: usize,
    ) -> Result<Arc<Self>> {
        std::fs::create_dir_all(path).map_err(|e| {
            Error::from(ErrorKind::Msg(format!(
                "kvdb_mdbx: failed to create dir {}: {}",
                path.display(),
                e
            )))
        })?;

        let mut builder = Database::<NoWriteMap>::new();
        builder.set_max_tables(DEFAULT_MAX_TABLES);
        builder.set_geometry(libmdbx::Geometry {
            // `Some(low..high)` means "current map size starts at `low`,
            // can grow to `high` on demand". Setting both to the same
            // value pins the size up front.
            size: Some(map_size_bytes..map_size_bytes),
            ..Default::default()
        });
        let db = builder.open(path).map_err(map_mdbx_error)?;
        Ok(Arc::new(Self { db }))
    }
}

/// Bridge a `libmdbx::Error` to the storage-crate `Error`.
fn map_mdbx_error(e: libmdbx::Error) -> Error {
    Error::from(ErrorKind::Msg(format!("mdbx: {}", e)))
}

/// Snapshot of storage occupancy for one MDBX column. All fields are
/// numeric so they can be pushed straight into a Prometheus gauge or
/// a status-endpoint response. Returned by [`KvdbMdbx::stats`].
///
/// The three page classes (branch / leaf / overflow) sum to the total
/// stored bytes at `page_size` granularity. `overflow_pages` growing
/// disproportionately is the early sign that value sizes are pushing
/// past what fits in a single leaf page — useful signal for revisiting
/// the encoding (see storage-design §3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KvdbMdbxStats {
    /// Fixed page size (usually 4 KiB); the same for every column.
    pub page_size_bytes: u32,
    /// B-tree height. Small integer; grows logarithmically with
    /// `entries`.
    pub btree_depth: u32,
    /// Internal (non-leaf) pages.
    pub branch_pages: usize,
    /// Leaf pages — where the actual key/value data lives.
    pub leaf_pages: usize,
    /// Overflow pages, used for values that don't fit in a single leaf.
    pub overflow_pages: usize,
    /// Number of stored key/value entries in this column.
    pub entries: usize,
    /// Bytes actually occupied on disk by this column (all page
    /// classes combined).
    pub bytes_used: u64,
}

/// Table name for a numeric column ID.
fn col_table_name(col: u32) -> String { format!("col-{}", col) }

/// One handle into one MDBX column. Cheap to `Clone` — shares the
/// underlying `Arc<MdbxEnv>`.
pub struct KvdbMdbx {
    pub env: Arc<MdbxEnv>,
    pub col: u32,
}

impl Clone for KvdbMdbx {
    fn clone(&self) -> Self {
        Self { env: Arc::clone(&self.env), col: self.col }
    }
}

impl MallocSizeOf for KvdbMdbx {
    fn size_of(&self, _ops: &mut MallocSizeOfOps) -> usize { 0 }
}

impl KvdbMdbx {
    /// Convenience: open an env and return a column-0 handle.
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self { env: MdbxEnv::open(path)?, col: 0 })
    }

    /// Construct a column handle against an already-opened env.
    pub fn with_column(env: Arc<MdbxEnv>, col: u32) -> Self {
        Self { env, col }
    }

    /// Ensure the named sub-table exists. Called lazily on first write.
    fn create_table(&self) -> Result<()> {
        let txn = self.env.db.begin_rw_txn().map_err(map_mdbx_error)?;
        txn.create_table(Some(&col_table_name(self.col)), TableFlags::default())
            .map_err(map_mdbx_error)?;
        txn.commit().map_err(map_mdbx_error)?;
        Ok(())
    }
}

// -------------------------- read path --------------------------

impl KeyValueDbTraitRead for KvdbMdbx {
    fn get(&self, key: &[u8]) -> Result<Option<Box<[u8]>>> {
        MDBX_GET_CALLS.inc(1);
        let txn = self.env.db.begin_ro_txn().map_err(map_mdbx_error)?;
        let table = match txn.open_table(Some(&col_table_name(self.col))) {
            Ok(t) => t,
            // NotFound is normal — the sub-table is created on first write.
            Err(libmdbx::Error::NotFound) => return Ok(None),
            Err(e) => return Err(map_mdbx_error(e)),
        };
        match txn.get::<Vec<u8>>(&table, key).map_err(map_mdbx_error)? {
            Some(v) => Ok(Some(v.into_boxed_slice())),
            None => Ok(None),
        }
    }
}

/// One entry in a batched write. `Put(k, v)` upserts; `Delete(k)`
/// removes (silently succeeds if the key is absent — matches
/// [`KeyValueDbTrait::delete`] semantics).
pub enum BatchOp<'a> {
    Put(&'a [u8], &'a [u8]),
    Delete(&'a [u8]),
}

impl KvdbMdbx {
    /// Apply a batch of writes inside a single MDBX `rw_txn`.
    ///
    /// The whole batch commits atomically — either every op lands or
    /// none of them do (on error, the txn is dropped). Compared to
    /// looping over [`KeyValueDbTrait::put`] this collapses N `rw_txn`
    /// begin/commit round-trips into one, which matters because MDBX
    /// serializes rw_txns globally: a state-commit that flushes 10k
    /// modified accounts one-put-at-a-time contends with itself all
    /// the way through, while the batched form takes the write lock
    /// once. In-tree consumer will be the executor's
    /// `apply_changes_to_storage` in
    /// `crates/dbs/statedb/src/lib.rs` — Phase 2 wiring.
    ///
    /// **Empty batch**: no-op, returns `Ok(())` without opening a txn.
    /// Cheap sanity check for callers that build a batch from a filter
    /// / iterator without knowing whether it will emit anything.
    ///
    /// **Ordering**: MDBX applies puts and deletes in the order given.
    /// If a caller submits `[Put(k, v1), Put(k, v2)]` the resulting
    /// stored value is `v2`. `[Put(k, v), Delete(k)]` leaves `k`
    /// absent. This matches the semantics of the batched transaction
    /// path (`KvdbMdbxTransaction::commit`) but is exposed as a
    /// single method for callers that don't need a hold-and-commit
    /// transaction object.
    /// Snapshot storage occupancy for this column. Wraps
    /// `libmdbx::Transaction::table_stat` on a fresh read txn so the
    /// caller gets a MVCC-consistent view without holding any lock
    /// after the call returns.
    ///
    /// A column that has never been written to has no sub-table on
    /// disk yet — we return a zero-filled `KvdbMdbxStats` instead of
    /// bubbling `libmdbx::Error::NotFound`, so operator dashboards can
    /// poll every declared column without racing against first-write
    /// creation.
    pub fn stats(&self) -> Result<KvdbMdbxStats> {
        let txn = self.env.db.begin_ro_txn().map_err(map_mdbx_error)?;
        let table = match txn.open_table(Some(&col_table_name(self.col)))
        {
            Ok(t) => t,
            Err(libmdbx::Error::NotFound) => {
                return Ok(KvdbMdbxStats {
                    page_size_bytes: 0,
                    btree_depth: 0,
                    branch_pages: 0,
                    leaf_pages: 0,
                    overflow_pages: 0,
                    entries: 0,
                    bytes_used: 0,
                });
            }
            Err(e) => return Err(map_mdbx_error(e)),
        };
        let stat =
            txn.table_stat(&table).map_err(map_mdbx_error)?;
        Ok(KvdbMdbxStats {
            page_size_bytes: stat.page_size(),
            btree_depth: stat.depth(),
            branch_pages: stat.branch_pages(),
            leaf_pages: stat.leaf_pages(),
            overflow_pages: stat.overflow_pages(),
            entries: stat.entries(),
            bytes_used: stat.total_size(),
        })
    }

    pub fn write_batch(&self, ops: &[BatchOp]) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        let _timer =
            MeterTimer::time_func(MDBX_BATCH_COMMIT_TIMER.as_ref());
        MDBX_BATCH_OPS_TOTAL.inc(ops.len());
        let txn =
            self.env.db.begin_rw_txn().map_err(map_mdbx_error)?;
        let table = txn
            .create_table(
                Some(&col_table_name(self.col)),
                TableFlags::default(),
            )
            .map_err(map_mdbx_error)?;
        for op in ops {
            match op {
                BatchOp::Put(k, v) => {
                    txn.put(&table, k, v, WriteFlags::empty())
                        .map_err(map_mdbx_error)?;
                }
                BatchOp::Delete(k) => {
                    let _ = txn
                        .del(&table, k, None)
                        .map_err(map_mdbx_error)?;
                }
            }
        }
        txn.commit().map_err(map_mdbx_error)?;
        Ok(())
    }

    /// Snapshot-consistent range scan. Reads a fresh MVCC snapshot at
    /// call time and returns every `(key, value)` pair in
    /// `[lower_bound_incl, upper_bound_excl)` in ascending key order.
    ///
    /// `upper_bound_excl = None` iterates to the end of the column.
    /// If the column has never been written to, returns an empty vector
    /// (matching the [`KeyValueDbTraitRead::get`] semantics for a
    /// missing sub-table).
    ///
    /// **Memory footprint**: results are materialized into `Vec` before
    /// return, so a range scan over a hot state column can be large.
    /// This is deliberate for the Phase 1 milestone — self-referential
    /// borrowed iterators over a `libmdbx` txn/cursor require careful
    /// lifetime management (`ouroboros` / unsafe pinning). We defer
    /// streaming iteration to Phase 2 when a real consumer needs it.
    /// The change-index range queries designed in
    /// `docs/internal/storage-design.md` §6.3 are bounded by construction
    /// (one bitmap lookup + one `AccountChangeSet` walk over the touched
    /// epochs), so the owned variant is enough for the Phase 1 goal:
    /// feature-parity smoke tests against `KvdbParitydb`.
    pub fn iter_range_owned(
        &self, lower_bound_incl: &[u8], upper_bound_excl: Option<&[u8]>,
    ) -> Result<Vec<(Box<[u8]>, Box<[u8]>)>> {
        let _timer =
            MeterTimer::time_func(MDBX_RANGE_SCAN_TIMER.as_ref());
        let txn = self.env.db.begin_ro_txn().map_err(map_mdbx_error)?;
        let table = match txn.open_table(Some(&col_table_name(self.col))) {
            Ok(t) => t,
            Err(libmdbx::Error::NotFound) => return Ok(Vec::new()),
            Err(e) => return Err(map_mdbx_error(e)),
        };
        let mut cursor = txn.cursor(&table).map_err(map_mdbx_error)?;

        let mut out: Vec<(Box<[u8]>, Box<[u8]>)> = Vec::new();
        let iter =
            cursor.iter_from::<Vec<u8>, Vec<u8>>(lower_bound_incl);
        for pair in iter {
            let (k, v) = pair.map_err(map_mdbx_error)?;
            if let Some(upper) = upper_bound_excl {
                if k.as_slice() >= upper {
                    break;
                }
            }
            out.push((
                k.into_boxed_slice(),
                v.into_boxed_slice(),
            ));
        }
        MDBX_RANGE_SCAN_ROWS.inc(out.len());
        Ok(out)
    }
}

mark_kvdb_multi_reader!(KvdbMdbx);

impl KeyValueDbTypes for KvdbMdbx {
    type ValueType = Box<[u8]>;
}

// -------------------------- write path --------------------------

impl KeyValueDbTrait for KvdbMdbx {
    fn delete(&self, key: &[u8]) -> Result<Option<Option<Box<[u8]>>>> {
        let txn = self.env.db.begin_rw_txn().map_err(map_mdbx_error)?;
        let table = txn
            .create_table(Some(&col_table_name(self.col)), TableFlags::default())
            .map_err(map_mdbx_error)?;
        let _ = txn.del(&table, key, None).map_err(map_mdbx_error)?;
        txn.commit().map_err(map_mdbx_error)?;
        Ok(None)
    }

    fn put(
        &self, key: &[u8], value: &[u8],
    ) -> Result<Option<Option<Box<[u8]>>>> {
        MDBX_PUT_CALLS.inc(1);
        let txn = self.env.db.begin_rw_txn().map_err(map_mdbx_error)?;
        let table = txn
            .create_table(Some(&col_table_name(self.col)), TableFlags::default())
            .map_err(map_mdbx_error)?;
        txn.put(&table, key, value, WriteFlags::empty())
            .map_err(map_mdbx_error)?;
        txn.commit().map_err(map_mdbx_error)?;
        Ok(None)
    }
}

// -------------------------- transactional path --------------------------

/// Buffer of pending operations. We replay them inside one MDBX rw_txn
/// at `commit()` time so a batch is atomic.
enum PendingOp {
    Put(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
}

pub struct KvdbMdbxTransaction {
    env: Arc<MdbxEnv>,
    col: u32,
    pending: Mutex<Vec<PendingOp>>,
}

impl KeyValueDbTypes for KvdbMdbxTransaction {
    type ValueType = Box<[u8]>;
}

impl KeyValueDbTraitOwnedRead for KvdbMdbxTransaction {
    fn get_mut(&mut self, _key: &[u8]) -> Result<Option<Box<[u8]>>> {
        // Matches KvdbParityDbTransaction: writes-only buffer, no
        // in-transaction reads. The caller reads through the parent
        // `KvdbMdbx` for the latest committed state.
        unreachable!("KvdbMdbxTransaction does not support reads — use the parent KvdbMdbx::get")
    }
}

impl KeyValueDbTraitSingleWriter for KvdbMdbxTransaction {
    fn delete(&mut self, key: &[u8]) -> Result<Option<Option<Box<[u8]>>>> {
        self.pending.lock().unwrap().push(PendingOp::Delete(key.to_vec()));
        Ok(None)
    }

    fn put(
        &mut self, key: &[u8], value: &[u8],
    ) -> Result<Option<Option<Box<[u8]>>>> {
        self.pending
            .lock()
            .unwrap()
            .push(PendingOp::Put(key.to_vec(), value.to_vec()));
        Ok(None)
    }
}

impl KeyValueDbTransactionTrait for KvdbMdbxTransaction {
    fn commit(&mut self, db: &dyn Any) -> Result<()> {
        let kvdb = match db.downcast_ref::<KvdbMdbx>() {
            Some(k) => k,
            None => bail!(
                "KvdbMdbxTransaction::commit: db is not a KvdbMdbx instance"
            ),
        };
        if !Arc::ptr_eq(&kvdb.env, &self.env) {
            bail!(
                "KvdbMdbxTransaction::commit: db env does not match the txn's env"
            );
        }
        if kvdb.col != self.col {
            bail!(
                "KvdbMdbxTransaction::commit: db col {} != txn col {}",
                kvdb.col,
                self.col
            );
        }

        let mut pending = self.pending.lock().unwrap();
        if pending.is_empty() { return Ok(()); }

        let txn = self.env.db.begin_rw_txn().map_err(map_mdbx_error)?;
        let table = txn
            .create_table(Some(&col_table_name(self.col)), TableFlags::default())
            .map_err(map_mdbx_error)?;
        for op in pending.drain(..) {
            match op {
                PendingOp::Put(k, v) => {
                    txn.put(&table, &k, &v, WriteFlags::empty())
                        .map_err(map_mdbx_error)?;
                }
                PendingOp::Delete(k) => {
                    // del returns Ok(false) when the key didn't exist,
                    // which is fine for a delete batch.
                    let _ = txn.del(&table, &k, None).map_err(map_mdbx_error)?;
                }
            }
        }
        txn.commit().map_err(map_mdbx_error)?;
        Ok(())
    }

    fn revert(&mut self) -> Result<()> {
        self.pending.lock().unwrap().clear();
        Ok(())
    }

    fn restart(
        &mut self, _immediate_write: bool, no_revert: bool,
    ) -> Result<()> {
        if !no_revert { self.revert()?; }
        Ok(())
    }
}

impl Drop for KvdbMdbxTransaction {
    fn drop(&mut self) {
        // Pending ops are dropped silently — matches KvdbParityDbTransaction.
    }
}

impl KeyValueDbTraitTransactional for KvdbMdbx {
    type TransactionType = KvdbMdbxTransaction;

    fn start_transaction(
        &self, _immediate_write: bool,
    ) -> Result<Self::TransactionType> {
        // Ensure the column sub-table exists so reads-during-transaction
        // via the parent handle find an empty table rather than a not-
        // found error.
        self.create_table()?;
        Ok(KvdbMdbxTransaction {
            env: Arc::clone(&self.env),
            col: self.col,
            pending: Mutex::new(Vec::new()),
        })
    }
}

impl DeltaDbTrait for KvdbMdbx {}

// -------------------------- smoke tests --------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::Any;

    /// Open → write → reopen → read. Verifies the MDBX env survives a
    /// clean shutdown/restart and round-trips byte payloads byte-for-byte.
    #[test]
    fn open_write_reopen_read() {
        let dir = tempdir::TempDir::new("kvdb_mdbx_smoke").unwrap();
        let path = dir.path();

        // First session: write a few entries across two columns.
        {
            let env = MdbxEnv::open(path).unwrap();
            let col0 = KvdbMdbx::with_column(Arc::clone(&env), 0);
            let col1 = KvdbMdbx::with_column(env, 1);
            col0.put(b"alpha", b"one").unwrap();
            col0.put(b"beta", b"two").unwrap();
            col1.put(b"gamma", b"three").unwrap();
        }

        // Second session: reopen, confirm values + column isolation.
        {
            let env = MdbxEnv::open(path).unwrap();
            let col0 = KvdbMdbx::with_column(Arc::clone(&env), 0);
            let col1 = KvdbMdbx::with_column(env, 1);
            assert_eq!(
                col0.get(b"alpha").unwrap().as_deref(),
                Some(&b"one"[..])
            );
            assert_eq!(
                col0.get(b"beta").unwrap().as_deref(),
                Some(&b"two"[..])
            );
            // Column 0 shouldn't see column 1's keys, and vice versa.
            assert!(col0.get(b"gamma").unwrap().is_none());
            assert!(col1.get(b"alpha").unwrap().is_none());
            assert_eq!(
                col1.get(b"gamma").unwrap().as_deref(),
                Some(&b"three"[..])
            );

            // Delete + verify absence.
            col0.delete(b"alpha").unwrap();
            assert!(col0.get(b"alpha").unwrap().is_none());
            assert_eq!(
                col0.get(b"beta").unwrap().as_deref(),
                Some(&b"two"[..])
            );
        }
    }

    /// `iter_range_owned` returns keys in ascending order, honors
    /// `[lower_incl, upper_excl)`, tolerates an empty column, and yields
    /// each key exactly once. Verifies the Phase 1 milestone: MDBX has
    /// range-scan parity with ParityDB at the KV level.
    #[test]
    fn iter_range_owned_semantics() {
        let dir =
            tempdir::TempDir::new("kvdb_mdbx_iter").unwrap();
        let env = MdbxEnv::open(dir.path()).unwrap();
        let col0 = KvdbMdbx::with_column(env, 0);

        // Empty column: iterating returns an empty vector, never errors.
        assert!(col0
            .iter_range_owned(b"", None)
            .unwrap()
            .is_empty());

        // Seed a set of keys — inserted out of order to prove the
        // returned iterator sorts by key, not by insertion order.
        let seed: [(&[u8], &[u8]); 5] = [
            (b"delta", b"4"),
            (b"alpha", b"1"),
            (b"echo", b"5"),
            (b"charlie", b"3"),
            (b"bravo", b"2"),
        ];
        for (k, v) in &seed {
            col0.put(k, v).unwrap();
        }

        // Full scan: every key, in ascending order.
        let full: Vec<(Box<[u8]>, Box<[u8]>)> =
            col0.iter_range_owned(b"", None).unwrap();
        let full_keys: Vec<&[u8]> =
            full.iter().map(|(k, _)| k.as_ref()).collect();
        assert_eq!(
            full_keys,
            vec![
                b"alpha".as_ref(),
                b"bravo".as_ref(),
                b"charlie".as_ref(),
                b"delta".as_ref(),
                b"echo".as_ref(),
            ]
        );
        // Values follow the keys correctly.
        assert_eq!(full[0].1.as_ref(), b"1");
        assert_eq!(full[4].1.as_ref(), b"5");

        // Half-open range [bravo, delta): expect bravo, charlie.
        let mid: Vec<(Box<[u8]>, Box<[u8]>)> =
            col0.iter_range_owned(b"bravo", Some(b"delta")).unwrap();
        let mid_keys: Vec<&[u8]> =
            mid.iter().map(|(k, _)| k.as_ref()).collect();
        assert_eq!(
            mid_keys,
            vec![b"bravo".as_ref(), b"charlie".as_ref()]
        );

        // Lower bound below first key, upper bound above last: full scan.
        let unbounded: Vec<(Box<[u8]>, Box<[u8]>)> =
            col0.iter_range_owned(b"", Some(b"zeta")).unwrap();
        assert_eq!(unbounded.len(), 5);

        // Empty range: lower == upper yields nothing.
        let empty: Vec<(Box<[u8]>, Box<[u8]>)> =
            col0.iter_range_owned(b"bravo", Some(b"bravo")).unwrap();
        assert!(empty.is_empty());

        // Lower bound above every key: empty result.
        let above: Vec<(Box<[u8]>, Box<[u8]>)> =
            col0.iter_range_owned(b"zeta", None).unwrap();
        assert!(above.is_empty());
    }

    /// `stats()` returns zeros on a never-written column and grows
    /// after writes. Verifies the operator-dashboard contract: probing
    /// a fresh column doesn't race against sub-table creation, and
    /// occupancy fields actually reflect the write volume.
    #[test]
    fn stats_zero_before_writes_grow_after() {
        let dir = tempdir::TempDir::new("kvdb_mdbx_stats").unwrap();
        let env = MdbxEnv::open(dir.path()).unwrap();
        let col0 = KvdbMdbx::with_column(env, 0);

        // Column never written: zeros, no error.
        let before = col0.stats().unwrap();
        assert_eq!(before.entries, 0);
        assert_eq!(before.leaf_pages, 0);
        assert_eq!(before.bytes_used, 0);

        // Ingest a batch — 300 keys with meaningful payload so leaf
        // pages get allocated. We build the (k, v) buffers first and
        // then borrow into BatchOp so the ops live for the batch call
        // (BatchOp holds slices).
        let owned: Vec<(Vec<u8>, Vec<u8>)> = (0u32..300)
            .map(|i| {
                (
                    i.to_be_bytes().to_vec(),
                    format!("payload-{}-with-some-body", i).into_bytes(),
                )
            })
            .collect();
        let ops: Vec<BatchOp> = owned
            .iter()
            .map(|(k, v)| BatchOp::Put(k.as_slice(), v.as_slice()))
            .collect();
        col0.write_batch(&ops).unwrap();

        let after = col0.stats().unwrap();
        assert_eq!(after.entries, 300);
        assert!(
            after.page_size_bytes >= 4096,
            "page size should be at least 4 KiB, got {}",
            after.page_size_bytes
        );
        assert!(
            after.leaf_pages >= 1,
            "at least one leaf page should be allocated for 300 entries"
        );
        assert!(after.bytes_used > 0);
        // Sanity: bytes_used = (branch + leaf + overflow) * page_size.
        let expected_bytes = (after.branch_pages
            + after.leaf_pages
            + after.overflow_pages) as u64
            * after.page_size_bytes as u64;
        assert_eq!(after.bytes_used, expected_bytes);
    }

    /// `write_batch`: empty batch is a no-op that returns `Ok(())`
    /// without touching the DB. Verifies the callers-with-empty-filter
    /// case doesn't accidentally create a spurious txn.
    #[test]
    fn write_batch_empty_is_noop() {
        let dir =
            tempdir::TempDir::new("kvdb_mdbx_batch_empty").unwrap();
        let env = MdbxEnv::open(dir.path()).unwrap();
        let col0 = KvdbMdbx::with_column(env, 0);
        // Populate one value to prove the empty batch doesn't disturb it.
        col0.put(b"unrelated", b"kept").unwrap();
        col0.write_batch(&[]).unwrap();
        assert_eq!(
            col0.get(b"unrelated").unwrap().as_deref(),
            Some(&b"kept"[..])
        );
    }

    /// `write_batch`: a mix of puts and deletes applies in order and
    /// atomically. In particular:
    ///   - later writes to the same key win (`[Put(k, v1), Put(k, v2)]`
    ///     → stored `v2`),
    ///   - a delete after a put erases (`[Put(k, v), Delete(k)]` → k
    ///     absent),
    ///   - a put after a delete resurrects (`[Delete(k), Put(k, v)]`
    ///     → stored `v`).
    #[test]
    fn write_batch_ordering() {
        let dir =
            tempdir::TempDir::new("kvdb_mdbx_batch_order").unwrap();
        let env = MdbxEnv::open(dir.path()).unwrap();
        let col0 = KvdbMdbx::with_column(env, 0);

        col0.put(b"pre-existing", b"prev").unwrap();

        col0.write_batch(&[
            BatchOp::Put(b"a", b"1"),
            BatchOp::Put(b"a", b"2"),
            BatchOp::Put(b"b", b"one"),
            BatchOp::Delete(b"b"),
            BatchOp::Delete(b"c"), // absent — silent success
            BatchOp::Delete(b"pre-existing"),
            BatchOp::Put(b"pre-existing", b"resurrected"),
        ])
        .unwrap();

        assert_eq!(col0.get(b"a").unwrap().as_deref(), Some(&b"2"[..]));
        assert!(col0.get(b"b").unwrap().is_none());
        assert!(col0.get(b"c").unwrap().is_none());
        assert_eq!(
            col0.get(b"pre-existing").unwrap().as_deref(),
            Some(&b"resurrected"[..])
        );
    }

    /// `write_batch`: many puts in one call produce the same final
    /// state as the same puts issued one-by-one. This is the operational
    /// substitution the executor will make in Phase 2 — flush thousands
    /// of state modifications per epoch in a single MDBX txn instead of
    /// one txn per key — so the equivalence must be exact.
    #[test]
    fn write_batch_matches_individual_puts() {
        let dir_a = tempdir::TempDir::new("mdbx_ind").unwrap();
        let dir_b = tempdir::TempDir::new("mdbx_bat").unwrap();
        let ind = KvdbMdbx::with_column(
            MdbxEnv::open(dir_a.path()).unwrap(),
            0,
        );
        let bat = KvdbMdbx::with_column(
            MdbxEnv::open(dir_b.path()).unwrap(),
            0,
        );

        // Same 200-key workload driven two ways.
        let keys: Vec<[u8; 4]> =
            (0u32..200).map(|i| i.to_be_bytes()).collect();
        let values: Vec<Vec<u8>> = (0u32..200)
            .map(|i| format!("val-{}", i).into_bytes())
            .collect();

        for (k, v) in keys.iter().zip(values.iter()) {
            ind.put(k, v).unwrap();
        }

        let ops: Vec<BatchOp> = keys
            .iter()
            .zip(values.iter())
            .map(|(k, v)| BatchOp::Put(k.as_slice(), v.as_slice()))
            .collect();
        bat.write_batch(&ops).unwrap();

        // Full-column scans must be identical.
        let scan_ind = ind.iter_range_owned(b"", None).unwrap();
        let scan_bat = bat.iter_range_owned(b"", None).unwrap();
        assert_eq!(
            scan_ind, scan_bat,
            "batched writes produced a different final state than individual puts"
        );
    }

    /// Fuzz-style parity test. Runs a deterministic randomized workload
    /// of put/delete/get/range_scan against both `KvdbMdbx` and an
    /// in-memory `BTreeMap` reference. After every op, MDBX's observable
    /// state must match the map byte-for-byte:
    ///   - `get(k)` returns the same value (or the same absence)
    ///   - `iter_range_owned(lo, hi)` returns the same (k, v) sequence
    ///     in the same order
    ///
    /// The reference `BTreeMap` is the semantic definition of what a
    /// KV store with ordered range scan should do — matching it means
    /// MDBX is a drop-in replacement for anything sitting behind the
    /// `KeyValueDbTraitRead` + `iter_range_owned` surface. This is the
    /// Phase 1 feature-parity gate: with it green, Phase 2 can start
    /// swapping ParityDB tables to MDBX without worrying about a KV-
    /// semantic mismatch at the swap boundary.
    ///
    /// Deterministic seed so failures are reproducible without an RNG
    /// dance. Workload sized to exercise range scans across a mix of
    /// present/absent keys and covers all three interleavings
    /// (insert-scan, delete-scan, scan-through-mutation-window).
    #[test]
    fn parity_against_btreemap_reference() {
        use std::collections::BTreeMap;

        // Tiny linear-congruential PRNG. Deterministic, dependency-free,
        // good enough to shake out edge cases without pulling `rand`
        // into a test-only path.
        struct Lcg(u64);
        impl Lcg {
            fn next_u32(&mut self) -> u32 {
                // Numerical Recipes constants (Park & Miller variant).
                self.0 = self
                    .0
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (self.0 >> 32) as u32
            }
            fn key(&mut self, keyspace_size: u32) -> [u8; 4] {
                (self.next_u32() % keyspace_size).to_be_bytes()
            }
            fn value(&mut self) -> Vec<u8> {
                // 8..40 random bytes.
                let len = 8 + (self.next_u32() % 32) as usize;
                (0..len)
                    .map(|_| (self.next_u32() & 0xff) as u8)
                    .collect()
            }
        }

        let dir =
            tempdir::TempDir::new("kvdb_mdbx_parity").unwrap();
        let env = MdbxEnv::open(dir.path()).unwrap();
        let mdbx = KvdbMdbx::with_column(env, 0);
        let mut reference: BTreeMap<Vec<u8>, Vec<u8>> =
            BTreeMap::new();

        let mut rng = Lcg(0xC0FFEE_C0FFEE);
        // Small keyspace (256) forces frequent collisions so both
        // "overwrite existing" and "delete existing" get exercised.
        // 5000 ops is enough to bury regressions but stays under 1s.
        const KEYSPACE: u32 = 256;
        const OPS: u32 = 5000;

        for _ in 0..OPS {
            // Op mix: 50% put, 20% get, 15% delete, 15% range_scan.
            let op = rng.next_u32() % 100;
            if op < 50 {
                // put
                let k = rng.key(KEYSPACE);
                let v = rng.value();
                mdbx.put(&k, &v).unwrap();
                reference.insert(k.to_vec(), v);
            } else if op < 70 {
                // get — same result on both sides.
                let k = rng.key(KEYSPACE);
                let mdbx_val: Option<Box<[u8]>> =
                    mdbx.get(&k).unwrap();
                let ref_val: Option<&Vec<u8>> = reference.get(&k[..]);
                match (mdbx_val, ref_val) {
                    (Some(m), Some(r)) => {
                        assert_eq!(&*m, r.as_slice(), "get mismatch at key {:?}", k);
                    }
                    (None, None) => {}
                    (m, r) => panic!(
                        "get presence mismatch at {:?}: mdbx={:?} ref={:?}",
                        k, m, r
                    ),
                }
            } else if op < 85 {
                // delete — remove from both, don't care about the
                // return value (both backends return None for us).
                let k = rng.key(KEYSPACE);
                mdbx.delete(&k).unwrap();
                reference.remove(&k[..]);
            } else {
                // range_scan — pick a lower/upper bound, compare the
                // full ordered sequence of (k, v) pairs.
                let lo = rng.key(KEYSPACE);
                let hi = rng.key(KEYSPACE);
                let (lo_key, hi_key) = if lo <= hi { (lo, hi) } else { (hi, lo) };
                let mdbx_scan: Vec<(Vec<u8>, Vec<u8>)> = mdbx
                    .iter_range_owned(&lo_key, Some(&hi_key))
                    .unwrap()
                    .into_iter()
                    .map(|(k, v)| (k.into_vec(), v.into_vec()))
                    .collect();
                let ref_scan: Vec<(Vec<u8>, Vec<u8>)> = reference
                    .range(lo_key.to_vec()..hi_key.to_vec())
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                assert_eq!(
                    mdbx_scan.len(),
                    ref_scan.len(),
                    "range size mismatch on [{:?}, {:?}): mdbx={} ref={}",
                    lo_key,
                    hi_key,
                    mdbx_scan.len(),
                    ref_scan.len(),
                );
                for (m, r) in mdbx_scan.iter().zip(ref_scan.iter()) {
                    assert_eq!(
                        m, r,
                        "range item mismatch on [{:?}, {:?})",
                        lo_key, hi_key
                    );
                }
            }
        }

        // Sanity check the final full-column scan matches the reference
        // exactly. If the per-op checks above pass, this is a
        // belt-and-suspenders end state assertion.
        let final_scan: Vec<(Vec<u8>, Vec<u8>)> = mdbx
            .iter_range_owned(b"", None)
            .unwrap()
            .into_iter()
            .map(|(k, v)| (k.into_vec(), v.into_vec()))
            .collect();
        let ref_scan: Vec<(Vec<u8>, Vec<u8>)> = reference
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        assert_eq!(final_scan, ref_scan, "final full-scan divergence");
    }

    /// Iterator sees a consistent MVCC snapshot: writes committed
    /// concurrently (from a fresh txn) after the range scan begins are
    /// not observed. The libmdbx read txn holds the snapshot open for
    /// the iterator's lifetime; the owned `Vec<>` return type is built
    /// from that snapshot.
    #[test]
    fn iter_range_owned_snapshot_isolation() {
        let dir =
            tempdir::TempDir::new("kvdb_mdbx_iter_iso").unwrap();
        let env = MdbxEnv::open(dir.path()).unwrap();
        let col0 = KvdbMdbx::with_column(env, 0);

        col0.put(b"a", b"1").unwrap();
        col0.put(b"b", b"2").unwrap();

        // First scan captures the initial two entries.
        let snap1 = col0.iter_range_owned(b"", None).unwrap();
        assert_eq!(snap1.len(), 2);

        // Concurrent writer inserts a third entry.
        col0.put(b"c", b"3").unwrap();

        // Snap1 was already materialized — still 2 entries.
        assert_eq!(snap1.len(), 2);

        // A fresh scan sees the new entry.
        let snap2 = col0.iter_range_owned(b"", None).unwrap();
        assert_eq!(snap2.len(), 3);
    }

    /// Batched transaction commits atomically. Mid-batch reads through
    /// the parent handle see the *pre-commit* state; only after
    /// `commit` does the parent see the new values.
    #[test]
    fn transaction_commits_atomically() {
        let dir =
            tempdir::TempDir::new("kvdb_mdbx_txn_smoke").unwrap();
        let env = MdbxEnv::open(dir.path()).unwrap();
        let col0 = KvdbMdbx::with_column(env, 0);

        // Seed one value via the non-transactional path.
        col0.put(b"keep", b"original").unwrap();

        let mut txn = col0.start_transaction(false).unwrap();
        KeyValueDbTraitSingleWriter::put(&mut txn, b"keep", b"changed")
            .unwrap();
        KeyValueDbTraitSingleWriter::put(&mut txn, b"new", b"value")
            .unwrap();
        KeyValueDbTraitSingleWriter::delete(&mut txn, b"missing").unwrap();
        // Pre-commit: parent handle still sees the original value.
        assert_eq!(
            col0.get(b"keep").unwrap().as_deref(),
            Some(&b"original"[..])
        );
        assert!(col0.get(b"new").unwrap().is_none());

        let as_any: &dyn Any = &col0;
        txn.commit(as_any).unwrap();

        // Post-commit: parent sees the updated state.
        assert_eq!(
            col0.get(b"keep").unwrap().as_deref(),
            Some(&b"changed"[..])
        );
        assert_eq!(
            col0.get(b"new").unwrap().as_deref(),
            Some(&b"value"[..])
        );
    }
}
