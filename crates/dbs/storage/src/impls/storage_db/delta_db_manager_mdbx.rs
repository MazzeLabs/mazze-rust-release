// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! MDBX-native `DeltaDbManagerTrait` implementation (Storage
//! Phase 4c).
//!
//! Under ParityDB, one delta MPT lived in one paritydb env per
//! snapshot at `storage_db/delta_mpts/paritydb_<hex_id>/`. This
//! module consolidates all delta MPTs into a single shared MDBX
//! column ([`MdbxColumn::DeltaMpt`](super::mdbx_columns::Column
//! ::DeltaMpt)) with per-snapshot isolation via a 32-byte
//! `EpochId` prefix, addressed through [`PrefixedKvdbMdbx`].
//!
//! Lifecycle:
//! - `new_empty_delta_db(name)` registers a per-snapshot metrics
//!   bundle and hands back a fresh `PrefixedKvdbMdbx`. No disk
//!   allocation — snapshots are logical slices of the shared
//!   column, materialised on first write.
//! - `get_delta_db(name)` probes the shared column for at least
//!   one key with the snapshot's prefix; returns `Some` iff so.
//!   The manager also caches metrics per snapshot so re-opens
//!   don't reset the counters.
//! - `destroy_delta_db(name)` fires a cursor-driven range delete
//!   on `[prefix, prefix + 1)` in one rw_txn, then evicts the
//!   metrics bundle.
//!
//! See
//! [`docs/internal/storage-delta-mpt-migration.md`](../../../../../docs/internal/storage-delta-mpt-migration.md)
//! for design rationale and cutover story.

use super::{
    kvdb_mdbx::{KvdbMdbx, MdbxEnv},
    mdbx_columns::Column,
    prefixed_kvdb_mdbx::{
        DeltaMptSnapshotMetrics, PrefixedKvdbMdbx, PREFIX_LEN,
    },
};
use crate::{
    impls::errors::*,
    storage_db::{
        delta_db_manager::DeltaDbManagerTrait, SnapshotInfo,
        SnapshotKeptToProvideSyncStatus,
    },
};
use parking_lot::RwLock;
use primitives::EpochId;
use rustc_hex::{FromHex, ToHex};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct DeltaDbManagerMdbx {
    /// Shared MDBX env, opened by `StorageManager` and injected
    /// here at construction.
    env: Arc<MdbxEnv>,
    /// Kept for API-shape parity with `DeltaDbManagerParitydb`
    /// (`get_delta_db_dir` returns it). No files are actually
    /// created under this dir — the MDBX column lives in the
    /// shared env's dir.
    delta_db_path: PathBuf,
    /// Per-snapshot metrics registry. Keyed by hex EpochId so
    /// re-opening a snapshot with `get_delta_db` reuses the same
    /// counters (Prometheus history isn't reset on reopen).
    /// Evicted at `destroy_delta_db`.
    snapshot_metrics: RwLock<HashMap<EpochId, Arc<DeltaMptSnapshotMetrics>>>,
}

impl DeltaDbManagerMdbx {
    /// Directory prefix kept identical to the paritydb manager's
    /// so operator log lines and metrics filenames stay
    /// greppable across the cutover. See §2.3 in the migration
    /// design doc.
    pub const DELTA_DB_MDBX_DIR_PREFIX: &'static str = "paritydb_";

    pub fn new(env: Arc<MdbxEnv>, delta_db_path: PathBuf) -> Result<Self> {
        // Create the directory even though we don't put anything
        // in it: `DeltaDbManagerTrait::get_delta_db_dir()` is
        // consumed by callers that expect a real path (log lines,
        // `path.exists()` checks in operator scripts). Cheap; the
        // dir stays empty on MDBX-native.
        if !delta_db_path.exists() {
            fs::create_dir_all(&delta_db_path)?;
        }
        Ok(Self {
            env,
            delta_db_path,
            snapshot_metrics: RwLock::new(HashMap::new()),
        })
    }

    /// Parse the epoch id out of a delta db name of the form
    /// `"paritydb_" + hex(EpochId)`. Errors on any format
    /// deviation — the manager only accepts names it produced.
    fn parse_epoch_id(name: &str) -> Result<EpochId> {
        let hex_part = name
            .strip_prefix(Self::DELTA_DB_MDBX_DIR_PREFIX)
            .ok_or_else(|| {
                ErrorKind::Msg(format!(
                    "DeltaDbManagerMdbx: delta_db_name {:?} \
                     doesn't start with expected prefix {:?}",
                    name,
                    Self::DELTA_DB_MDBX_DIR_PREFIX
                ))
            })?;
        let bytes: Vec<u8> = hex_part.from_hex().map_err(|e| {
            ErrorKind::Msg(format!(
                "DeltaDbManagerMdbx: delta_db_name {:?} tail is \
                 not valid hex: {}",
                name, e
            ))
        })?;
        if bytes.len() != PREFIX_LEN {
            bail!(
                "DeltaDbManagerMdbx: delta_db_name {:?} decodes to \
                 {} bytes, expected {}",
                name,
                bytes.len(),
                PREFIX_LEN
            );
        }
        let mut out = [0u8; PREFIX_LEN];
        out.copy_from_slice(&bytes);
        Ok(EpochId::from_slice(&out))
    }

    /// Metrics group for one snapshot. Stable across restarts as
    /// long as the epoch id doesn't change.
    fn metrics_group(epoch_id: &EpochId) -> String {
        format!(
            "mdbx_delta_mpt.{}",
            epoch_id.as_ref().to_hex::<String>()
        )
    }

    /// Fetch (or lazily register) the metrics bundle for a
    /// snapshot. Registration is idempotent within one process
    /// lifetime — reopening a snapshot returns the same counters.
    fn metrics_for(
        &self, epoch_id: &EpochId,
    ) -> Arc<DeltaMptSnapshotMetrics> {
        {
            let map = self.snapshot_metrics.read();
            if let Some(m) = map.get(epoch_id) {
                return Arc::clone(m);
            }
        }
        let mut map = self.snapshot_metrics.write();
        // Re-check after upgrading — another thread may have
        // registered between our read release and write acquire.
        if let Some(m) = map.get(epoch_id) {
            return Arc::clone(m);
        }
        let m = Arc::new(DeltaMptSnapshotMetrics::register(
            &Self::metrics_group(epoch_id),
        ));
        map.insert(*epoch_id, Arc::clone(&m));
        m
    }

    /// Prefix bytes for one snapshot. The EpochId is 32 bytes so
    /// this is a direct copy.
    fn prefix_for(epoch_id: &EpochId) -> [u8; PREFIX_LEN] {
        let mut out = [0u8; PREFIX_LEN];
        out.copy_from_slice(epoch_id.as_ref());
        out
    }

    /// Fresh `PrefixedKvdbMdbx` for a snapshot, with metrics
    /// bundle attached.
    fn open_handle(&self, epoch_id: &EpochId) -> PrefixedKvdbMdbx {
        let kvdb = KvdbMdbx::with_column(
            Arc::clone(&self.env),
            Column::DeltaMpt.id(),
        );
        PrefixedKvdbMdbx::new_metered(
            kvdb,
            Self::prefix_for(epoch_id),
            self.metrics_for(epoch_id),
        )
    }

    /// Cheap existence probe: does the shared column carry at
    /// least one key with this prefix? MDBX iter_range is O(log N)
    /// to seek + O(1) for the first entry — cheap enough for the
    /// per-startup `scan_persist_state` walk.
    fn prefix_has_any(&self, epoch_id: &EpochId) -> Result<bool> {
        let kvdb = KvdbMdbx::with_column(
            Arc::clone(&self.env),
            Column::DeltaMpt.id(),
        );
        let prefix = Self::prefix_for(epoch_id);
        // Read one entry starting at `prefix`. If it comes back
        // and is still inside the snapshot's slice, we have data.
        let hits = kvdb.iter_range_owned(
            &prefix[..],
            Self::upper_bound_bytes(&prefix).as_deref(),
        )?;
        Ok(!hits.is_empty())
    }

    /// The exclusive upper bound `[prefix + 1]` used for prefix-
    /// scoped range scans/deletes. `None` when `prefix` is
    /// `0xff…ff` (there's no representable next value; caller
    /// should treat as unbounded).
    fn upper_bound_bytes(prefix: &[u8; PREFIX_LEN]) -> Option<Vec<u8>> {
        let mut ub = *prefix;
        for byte in ub.iter_mut().rev() {
            if *byte == 0xff {
                *byte = 0;
            } else {
                *byte += 1;
                return Some(ub.to_vec());
            }
        }
        None
    }
}

impl DeltaDbManagerTrait for DeltaDbManagerMdbx {
    type DeltaDb = PrefixedKvdbMdbx;

    /// Override the default `scan_persist_state`: under the
    /// paritydb backend the trait's default impl scanned the
    /// `delta_db_dir` for stray subdirectories to delete. We
    /// don't create per-snapshot dirs, so the whole
    /// `fs::read_dir` walk becomes meaningless (and worse,
    /// panics with "NotFound" on the first boot before anything
    /// has touched the dir). Query MDBX by prefix instead — that
    /// IS the source of truth for which delta MPTs exist under
    /// this backend.
    ///
    /// See `docs/internal/storage-delta-mpt-migration.md` §6.4.
    fn scan_persist_state(
        &self, snapshot_info_map: &HashMap<EpochId, SnapshotInfo>,
    ) -> Result<(Vec<EpochId>, HashMap<EpochId, Self::DeltaDb>)> {
        // Same expected-set assembly as the default impl —
        // primary delta MPT for each snapshot, plus intermediate
        // delta MPT keyed by the parent snapshot.
        let mut expected: HashMap<EpochId, ()> = HashMap::new();
        for (snapshot_epoch_id, snapshot_info) in snapshot_info_map {
            expected.insert(snapshot_epoch_id.clone(), ());
            expected.insert(
                snapshot_info.parent_snapshot_epoch_id.clone(),
                (),
            );
        }

        let mut delta_mpts = HashMap::new();
        for epoch_id in expected.keys() {
            let name = self.get_delta_db_name(epoch_id);
            if let Some(handle) = self.get_delta_db(&name)? {
                delta_mpts.insert(*epoch_id, handle);
            }
        }

        let mut missing_delta_dbs = vec![];
        for (snapshot_epoch_id, snapshot_info) in snapshot_info_map {
            if snapshot_info.snapshot_info_kept_to_provide_sync
                == SnapshotKeptToProvideSyncStatus::No
                && !delta_mpts.contains_key(snapshot_epoch_id)
            {
                missing_delta_dbs.push(snapshot_epoch_id.clone());
            }
        }

        Ok((missing_delta_dbs, delta_mpts))
    }

    fn get_delta_db_dir(&self) -> &Path {
        self.delta_db_path.as_path()
    }

    fn get_delta_db_name(&self, snapshot_epoch_id: &EpochId) -> String {
        // Same shape as `DeltaDbManagerParitydb::get_delta_db_name`
        // so log lines and dashboards keep parsing across the
        // cutover.
        Self::DELTA_DB_MDBX_DIR_PREFIX.to_string()
            + &snapshot_epoch_id.as_ref().to_hex::<String>()
    }

    fn get_delta_db_path(&self, delta_db_name: &str) -> PathBuf {
        // Kept for scan_persist_state's "delete extra dir" logic;
        // under MDBX we never create anything at this path.
        self.delta_db_path.join(delta_db_name)
    }

    fn new_empty_delta_db(
        &self, delta_db_name: &str,
    ) -> Result<Self::DeltaDb> {
        let epoch_id = Self::parse_epoch_id(delta_db_name)?;
        // No disk allocation — the snapshot's slice materialises
        // on the first put(). We still register metrics eagerly so
        // the "empty" state shows in dashboards.
        Ok(self.open_handle(&epoch_id))
    }

    fn get_delta_db(
        &self, delta_db_name: &str,
    ) -> Result<Option<Self::DeltaDb>> {
        let epoch_id = Self::parse_epoch_id(delta_db_name)?;
        if self.prefix_has_any(&epoch_id)? {
            Ok(Some(self.open_handle(&epoch_id)))
        } else {
            Ok(None)
        }
    }

    fn destroy_delta_db(&self, delta_db_name: &str) -> Result<()> {
        let epoch_id = Self::parse_epoch_id(delta_db_name)?;
        let prefix = Self::prefix_for(&epoch_id);
        let upper = Self::upper_bound_bytes(&prefix);
        let kvdb = KvdbMdbx::with_column(
            Arc::clone(&self.env),
            Column::DeltaMpt.id(),
        );
        let n = kvdb.delete_range(&prefix[..], upper.as_deref())?;
        // Evict metrics — the snapshot is gone. Dashboards will
        // stop seeing new increments; the last observed values
        // stay in the registry until the process restarts.
        self.snapshot_metrics.write().remove(&epoch_id);
        debug!(
            "DeltaDbManagerMdbx: destroyed snapshot {} ({} keys \
             removed)",
            epoch_id.as_ref().to_hex::<String>(),
            n
        );
        Ok(())
    }
}

// -------------------------- tests --------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage_db::key_value_db::{
        KeyValueDbTrait, KeyValueDbTraitRead,
    };
    use tempdir::TempDir;

    fn open_env() -> (TempDir, Arc<MdbxEnv>) {
        let dir = TempDir::new("delta_db_manager_mdbx").unwrap();
        let env = MdbxEnv::open(dir.path()).unwrap();
        (dir, env)
    }

    fn make_manager() -> (TempDir, TempDir, DeltaDbManagerMdbx) {
        let (env_dir, env) = open_env();
        let path_dir = TempDir::new("delta_mpts_path").unwrap();
        let m = DeltaDbManagerMdbx::new(
            env,
            path_dir.path().to_path_buf(),
        )
        .unwrap();
        (env_dir, path_dir, m)
    }

    fn epoch_id(byte: u8) -> EpochId {
        EpochId::from_slice(&[byte; PREFIX_LEN])
    }

    /// `get_delta_db_name` matches the paritydb manager's format
    /// exactly — dashboards greppable across cutover.
    #[test]
    fn delta_db_name_format_matches_paritydb() {
        let (_e, _p, m) = make_manager();
        let id = epoch_id(0xab);
        let name = m.get_delta_db_name(&id);
        assert!(name.starts_with("paritydb_"));
        assert_eq!(
            name,
            "paritydb_".to_string() + &"ab".repeat(PREFIX_LEN)
        );
    }

    /// `parse_epoch_id` round-trips with `get_delta_db_name`.
    #[test]
    fn epoch_id_round_trips_through_name() {
        let (_e, _p, m) = make_manager();
        let id = epoch_id(0x5a);
        let name = m.get_delta_db_name(&id);
        assert_eq!(DeltaDbManagerMdbx::parse_epoch_id(&name).unwrap(), id);
    }

    /// Bad names fail loudly.
    #[test]
    fn parse_epoch_id_rejects_malformed_names() {
        assert!(DeltaDbManagerMdbx::parse_epoch_id("wrong").is_err());
        assert!(
            DeltaDbManagerMdbx::parse_epoch_id("paritydb_zz").is_err()
        );
        // Right prefix + hex but wrong length
        assert!(DeltaDbManagerMdbx::parse_epoch_id("paritydb_abcd")
            .is_err());
    }

    /// `new_empty_delta_db` returns a handle; the caller can put
    /// keys; `get_delta_db` sees them; other snapshots don't.
    #[test]
    fn new_empty_get_and_isolate() {
        let (_e, _p, m) = make_manager();
        let a_name = m.get_delta_db_name(&epoch_id(0x01));
        let b_name = m.get_delta_db_name(&epoch_id(0x02));

        let a = m.new_empty_delta_db(&a_name).unwrap();
        a.put(b"key", b"a-val").unwrap();

        // `get_delta_db` for `a` sees it now.
        let a_reopen = m.get_delta_db(&a_name).unwrap();
        assert!(a_reopen.is_some());
        assert_eq!(
            a_reopen.unwrap().get(b"key").unwrap().as_deref(),
            Some(&b"a-val"[..])
        );

        // `b` is empty — get_delta_db returns None per the
        // "probe for any key" contract.
        assert!(m.get_delta_db(&b_name).unwrap().is_none());
    }

    /// `destroy_delta_db` purges every key with the snapshot's
    /// prefix and leaves neighbouring snapshots untouched.
    #[test]
    fn destroy_purges_only_target_prefix() {
        let (_e, _p, m) = make_manager();
        let a_name = m.get_delta_db_name(&epoch_id(0xa0));
        let b_name = m.get_delta_db_name(&epoch_id(0xb0));

        let a = m.new_empty_delta_db(&a_name).unwrap();
        let b = m.new_empty_delta_db(&b_name).unwrap();
        for i in 0u8..16 {
            a.put(&[i], &[i, i]).unwrap();
            b.put(&[i], &[i, i]).unwrap();
        }

        m.destroy_delta_db(&a_name).unwrap();

        // `a` is gone (probe returns None).
        assert!(m.get_delta_db(&a_name).unwrap().is_none());
        // `b` is intact.
        let b_reopen = m.get_delta_db(&b_name).unwrap().unwrap();
        for i in 0u8..16 {
            assert_eq!(
                b_reopen.get(&[i]).unwrap().as_deref(),
                Some(&[i, i][..])
            );
        }
    }

    /// Metrics bundle is reused across re-opens of the same
    /// snapshot; separate snapshots get separate bundles.
    #[test]
    fn metrics_bundle_is_stable_per_snapshot() {
        let (_e, _p, m) = make_manager();
        let a_name = m.get_delta_db_name(&epoch_id(0xc1));
        let b_name = m.get_delta_db_name(&epoch_id(0xc2));

        let a1 = m.new_empty_delta_db(&a_name).unwrap();
        a1.put(b"k", b"v").unwrap();
        let a2 = m.get_delta_db(&a_name).unwrap().unwrap();
        // Both handles should point at the same registered bundle
        // (Arc equality). We can't test that directly through the
        // trait surface, but re-opening MUST return the same
        // Arc<DeltaMptSnapshotMetrics> from the registry.
        let map = m.snapshot_metrics.read();
        assert_eq!(map.len(), 1);
        assert!(map.contains_key(&epoch_id(0xc1)));
        drop(map);

        // Registering a second snapshot yields a distinct bundle.
        let _b = m.new_empty_delta_db(&b_name).unwrap();
        let map = m.snapshot_metrics.read();
        assert_eq!(map.len(), 2);
        assert!(map.contains_key(&epoch_id(0xc2)));

        // Silence unused-var lints for the ergonomic handle names.
        let _ = (a1, a2);
    }

    /// `destroy_delta_db` evicts the metrics bundle so a
    /// subsequent `new_empty_delta_db` for the same id registers
    /// fresh counters (matching the observable behaviour that a
    /// destroyed-then-recreated snapshot is a clean slate).
    #[test]
    fn destroy_evicts_metrics_bundle() {
        let (_e, _p, m) = make_manager();
        let name = m.get_delta_db_name(&epoch_id(0xd0));
        let _handle = m.new_empty_delta_db(&name).unwrap();
        assert_eq!(m.snapshot_metrics.read().len(), 1);
        m.destroy_delta_db(&name).unwrap();
        assert_eq!(m.snapshot_metrics.read().len(), 0);
    }
}
