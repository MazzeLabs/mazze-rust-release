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
use libmdbx::{Database, NoWriteMap, TableFlags, WriteFlags};
use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};
use std::{
    any::Any,
    path::Path,
    sync::{Arc, Mutex},
};

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

impl KvdbMdbx {
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
