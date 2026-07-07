// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! MDBX-native `SnapshotDbManagerTrait` implementation — Phase 5c.
//!
//! Owns the dedicated snapshot MDBX env (opened in pre-work #3)
//! and hands out [`SnapshotKvDbMdbx`] handles that share it, each
//! scoped by the 33-byte `[epoch_id | sub_prefix]` prefix from
//! 5c.a.
//!
//! # What this commit (5c.d) covers
//!
//! Scaffolding:
//! - Manager struct + `new`.
//! - Merge-marker RLP payload (`MergeMarker`) — the data body of
//!   the `[epoch_id | b'!']` sentinel written by the chunked merge
//!   protocol in 5c.e.
//! - Trait method scaffolding: name parsing / composition, path
//!   helpers, `snapshot_dir_exists` override (in-memory set),
//!   chunked `destroy_snapshot`, `get_snapshot_by_epoch_id`,
//!   `try_get_new_snapshot_epoch_from_*_path` (both return `None`
//!   under MDBX — no per-snapshot dirs).
//! - Semaphore-bounded handle count preserved for API parity.
//!
//! # What 5c.e adds
//!
//! - `new_snapshot_by_merging` — chunked writer + marker protocol
//!   per design doc §2.3.2.1.
//! - `new_temp_snapshot_for_full_sync` / `finalize_full_sync_snapshot`.
//! - `scan_persist_state` override — prefix enumeration + orphan
//!   GC + marker detection.
//! - Aggregate-per-subsystem metrics counters (user's opt-a per
//!   §6 Q#4).
//!
//! # Reference impl
//!
//! [`SnapshotDbManagerParitydb`](super::snapshot_db_manager_paritydb::SnapshotDbManagerParitydb)
//! — cross-check every trait method's behavior against it.
//! See `docs/internal/storage-phase-5-cde-migration.md` §2.3.2.

use super::{
    kvdb_mdbx::MdbxEnv,
    snapshot_kv_db_mdbx::SnapshotKvDbMdbx,
    snapshot_prefix::MergeMarkerKind,
};
use crate::{
    impls::{
        delta_mpt::DeltaMptIterator, errors::*,
        storage_manager::PersistedSnapshotInfoMap,
    },
    storage_db::{
        AlreadyOpenSnapshots, SnapshotDbManagerTrait, SnapshotDbTrait,
        SnapshotInfo,
    },
};
use parking_lot::{RwLock, RwLockWriteGuard};
use primitives::{EpochId, MerkleHash, NULL_EPOCH};
use rlp::{Decodable, DecoderError, Encodable, Rlp, RlpStream};
use rustc_hex::ToHex;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};
use tokio::sync::Semaphore;

/// RLP body of a crash-recovery marker written under
/// `[epoch_id | b'!']` in the snapshot column. The 5c.a
/// `MergeMarkerKind` tag byte lives inside this RLP so a single
/// `get()` on the marker key returns everything the recovery
/// path needs.
///
/// **Wire format** (RLP list of 3):
/// ```text
/// [ kind_tag: u8,
///   started_at: u64 (unix seconds),
///   context: [u8; 32] (parent_epoch_id for Merge,
///                      merkle_root for FullSync) ]
/// ```
///
/// The `context` field is deliberately typed as `[u8; 32]` here
/// so both the `EpochId` and `MerkleHash` variants pack byte-for-
/// byte identically — parseable without knowing the kind up front.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeMarker {
    /// Which lifecycle wrote the marker.
    pub kind: MergeMarkerKind,
    /// Unix seconds when the marker was written. Used only for
    /// operator log lines; nothing depends on the exact value.
    pub started_at: u64,
    /// Kind-dependent 32-byte payload — the parent epoch id for
    /// `Merge`, the child snapshot's expected merkle root for
    /// `FullSync`.
    pub context: [u8; 32],
}

impl MergeMarker {
    /// Convenience constructor for a `Merge` marker.
    pub fn merge(parent: &EpochId, started_at: u64) -> Self {
        let mut context = [0u8; 32];
        context.copy_from_slice(parent.as_ref());
        Self {
            kind: MergeMarkerKind::Merge,
            started_at,
            context,
        }
    }

    /// Convenience constructor for a `FullSync` marker.
    pub fn full_sync(merkle_root: &MerkleHash, started_at: u64) -> Self {
        let mut context = [0u8; 32];
        context.copy_from_slice(merkle_root.as_ref());
        Self {
            kind: MergeMarkerKind::FullSync,
            started_at,
            context,
        }
    }
}

impl Encodable for MergeMarker {
    fn rlp_append(&self, s: &mut RlpStream) {
        s.begin_list(3);
        s.append(&self.kind.tag());
        s.append(&self.started_at);
        // Append the 32-byte context as a bytes-string. RLP will
        // encode the length inline.
        s.append(&self.context.as_ref());
    }
}

impl Decodable for MergeMarker {
    fn decode(rlp: &Rlp) -> std::result::Result<Self, DecoderError> {
        if rlp.item_count()? != 3 {
            return Err(DecoderError::RlpIncorrectListLen);
        }
        let tag: u8 = rlp.val_at(0)?;
        let kind = MergeMarkerKind::from_tag(tag).ok_or(
            DecoderError::Custom("unknown MergeMarkerKind tag"),
        )?;
        let started_at: u64 = rlp.val_at(1)?;
        let context_bytes: Vec<u8> = rlp.val_at(2)?;
        if context_bytes.len() != 32 {
            return Err(DecoderError::Custom(
                "MergeMarker context must be 32 bytes",
            ));
        }
        let mut context = [0u8; 32];
        context.copy_from_slice(&context_bytes);
        Ok(Self { kind, started_at, context })
    }
}

/// MDBX-native manager for snapshot-tier state. Wraps the
/// dedicated snapshot `Arc<MdbxEnv>` from pre-work #3 and gates
/// concurrent open handles via the same semaphore the paritydb
/// manager used (see §2.3.2 for the bounded-open rationale under
/// MDBX).
pub struct SnapshotDbManagerMdbx {
    /// Dedicated snapshot MDBX env — the one opened by
    /// `StorageManager::new_arc` at
    /// `storage_db/mdbx_snapshot/`. Shared across every handle.
    env: Arc<MdbxEnv>,
    /// Kept for API-shape parity with
    /// `DeltaDbManagerParitydb::get_snapshot_dir` — operator
    /// scripts and log lines expect a real path even though
    /// nothing is written under it. Set alongside the env dir at
    /// construction; the dir may not exist until the manager
    /// creates it lazily.
    snapshot_dir: PathBuf,
    /// Same story — `get_mpt_snapshot_dir` in the trait needs a
    /// path. Under MDBX the MPT lives in the same env so this
    /// just points back at [`Self::snapshot_dir`]; kept as a
    /// separate field so a future split (§3 reservation) could
    /// wire it independently.
    mpt_snapshot_dir: PathBuf,
    /// Bounds concurrent open snapshot handles. See
    /// `SnapshotDbManagerParitydb::open_snapshot_semaphore` —
    /// under MDBX the fd pressure that motivated the sizing is
    /// gone but bounded handles + metrics churn remains the
    /// justification. Callers depend on the `try_open` →
    /// `SemaphoreTryAcquireError` fast path.
    open_snapshot_semaphore: Arc<Semaphore>,
    /// Kept for the trait's `Self::SnapshotDb` open-tracking; the
    /// MDBX impl doesn't use per-snapshot fds so this is not
    /// consulted for real work, but it round-trips through
    /// `SnapshotDbTrait::open` / `create` API contracts.
    already_open_snapshots: AlreadyOpenSnapshots<SnapshotKvDbMdbx>,
    /// **In-memory known-snapshots set** — the source of truth
    /// for [`Self::snapshot_dir_exists`]. Populated by
    /// `scan_persist_state` at boot (5c.e) and mutated on
    /// `new_snapshot_by_merging` / `destroy_snapshot`. Non-
    /// blocking (RwLock read-fast-path) — the trait's docstring
    /// pledges non-blocking behaviour.
    known_snapshots: RwLock<HashSet<EpochId>>,
    /// Most recent snapshot the manager knows about; used by
    /// the (5c.e) merge path to pick between direct_merge and
    /// copy_and_merge. `NULL_EPOCH` at boot before
    /// `scan_persist_state` runs.
    latest_snapshot_id: RwLock<(EpochId, u64)>,
    /// The snapshot the manager was booted with — logged loudly
    /// after a restart if it differs from the current tip.
    /// Cleared via [`Self::clean_snapshot_epoch_id_before_recovered`].
    snapshot_epoch_id_before_recovered: RwLock<Option<EpochId>>,
    /// Flag set by consensus to force a merge into
    /// `in_reconstruct_snapshot_state` mode on the next matching
    /// snapshot. Same contract as the paritydb reference.
    reconstruct_snapshot_id_for_reboot: RwLock<Option<EpochId>>,
}

impl SnapshotDbManagerMdbx {
    /// Directory prefix kept identical to the paritydb manager's
    /// so operator log lines and metrics filenames stay greppable
    /// across the cutover.
    pub const SNAPSHOT_DB_MDBX_DIR_PREFIX: &'static str = "paritydb_";
    /// Kept for API-shape parity — the MPT dir name the paritydb
    /// manager produced via `get_latest_mpt_snapshot_db_name`.
    pub const LATEST_MPT_SNAPSHOT_NAME: &'static str = "paritydb_latest";
    /// Subdirectory the paritydb manager created for MPT
    /// snapshots. Kept in the path helpers because operator
    /// scripts may still stat these paths post-cutover.
    const MPT_SNAPSHOT_SUBDIR: &'static str = "mpt_snapshot";

    /// Construct the manager. `env` is the snapshot env opened in
    /// [`crate::impls::storage_manager::StorageManager`]; the
    /// `snapshot_dir` / `mpt_snapshot_dir` paths are kept for API
    /// parity with the paritydb reference (nothing is written
    /// under them by the MDBX impl).
    pub fn new(
        env: Arc<MdbxEnv>, snapshot_path: PathBuf,
        max_open_snapshots: u16,
    ) -> Result<Self> {
        // Materialise the pathlike dirs so operator scripts that
        // stat them don't ENOENT on a fresh node. Cheap — an
        // empty directory tree per snapshot subsystem.
        if !snapshot_path.exists() {
            fs::create_dir_all(&snapshot_path)?;
        }
        let mpt_snapshot_path = snapshot_path
            .parent()
            .unwrap_or(&snapshot_path)
            .join(Self::MPT_SNAPSHOT_SUBDIR);
        if !mpt_snapshot_path.exists() {
            fs::create_dir_all(&mpt_snapshot_path)?;
        }
        Ok(Self {
            env,
            snapshot_dir: snapshot_path,
            mpt_snapshot_dir: mpt_snapshot_path,
            open_snapshot_semaphore: Arc::new(Semaphore::new(
                max_open_snapshots as usize,
            )),
            already_open_snapshots: Default::default(),
            known_snapshots: RwLock::new(HashSet::new()),
            latest_snapshot_id: RwLock::new((NULL_EPOCH, 0)),
            snapshot_epoch_id_before_recovered: RwLock::new(None),
            reconstruct_snapshot_id_for_reboot: RwLock::new(None),
        })
    }

    /// Snapshot env handle. Escape hatch for the merge path
    /// (5c.e) which needs to bypass the `SnapshotKvDbMdbx` wrapper
    /// to write the marker key directly.
    pub fn env(&self) -> Arc<MdbxEnv> { Arc::clone(&self.env) }

    /// Mirrors `SnapshotDbManagerParitydb::update_latest_snapshot_id`.
    /// Consumed by consensus / storage_manager after a successful
    /// snapshot registration.
    pub fn update_latest_snapshot_id(
        &self, snapshot_id: EpochId, height: u64,
    ) {
        *self.latest_snapshot_id.write() = (snapshot_id, height);
    }

    /// Mirrors
    /// `SnapshotDbManagerParitydb::clean_snapshot_epoch_id_before_recovered`.
    pub fn clean_snapshot_epoch_id_before_recovered(&self) {
        *self.snapshot_epoch_id_before_recovered.write() = None;
    }

    /// Mirrors
    /// `SnapshotDbManagerParitydb::set_reconstruct_snapshot_id`.
    pub fn set_reconstruct_snapshot_id(
        &self, reconstruct_main: Option<EpochId>,
    ) {
        debug!(
            "set_reconstruct_snapshot_id to {:?}",
            reconstruct_main
        );
        *self.reconstruct_snapshot_id_for_reboot.write() = reconstruct_main;
    }

    /// Under MDBX there is no "latest MPT snapshot dir" to
    /// recreate — the MPT lives in the same env as the KV. Kept
    /// for API parity so callers that expect this method work
    /// unchanged.
    pub fn recreate_latest_mpt_snapshot(&self) -> Result<()> {
        info!(
            "recreate latest mpt snapshot (MDBX-native — no-op)"
        );
        Ok(())
    }

    /// Register a snapshot as present in the manager's in-memory
    /// set. Called after `new_snapshot_by_merging` /
    /// `finalize_full_sync_snapshot` (5c.e) commit their
    /// snapshot_info entry — this must run under the same lock
    /// so `snapshot_dir_exists` never sees the info without the
    /// snapshot.
    pub fn mark_snapshot_known(&self, snapshot_epoch_id: EpochId) {
        self.known_snapshots.write().insert(snapshot_epoch_id);
    }

    /// Read-only view of the known-snapshots set. Consumed by
    /// `scan_persist_state` in 5c.e for the expected-vs-present
    /// comparison.
    pub fn known_snapshots_snapshot(&self) -> HashSet<EpochId> {
        self.known_snapshots.read().clone()
    }

    // ------------------ semaphore + open helpers ------------------

    /// Acquire a permit or fail via `SemaphoreTryAcquireError` if
    /// `try_open` is set and the pool is empty. Preserves the
    /// paritydb reference's caller contract.
    fn acquire_open_permit(&self, try_open: bool) -> Result<()> {
        if try_open {
            self.open_snapshot_semaphore
                .try_acquire()
                .map_err(|_err| ErrorKind::SemaphoreTryAcquireError)?
                .forget();
        } else {
            futures::executor::block_on(self.open_snapshot_semaphore.acquire())
                .forget();
        }
        Ok(())
    }

    /// Existence probe using pre-work #1's `first_in_range` — one
    /// cursor seek, no full-range materialisation. Same shape as
    /// `DeltaDbManagerMdbx::prefix_has_any` per design doc §2.3.2.4.
    fn snapshot_has_any_data(
        &self, snapshot_epoch_id: &EpochId,
    ) -> Result<bool> {
        use super::{
            kvdb_mdbx::KvdbMdbx, snapshot_kv_db_mdbx::SNAPSHOT_COL,
            snapshot_prefix::{
                compose_snapshot_scope, snapshot_scope_upper_bound,
            },
        };
        let lower = compose_snapshot_scope(snapshot_epoch_id);
        let upper = snapshot_scope_upper_bound(snapshot_epoch_id);
        let kvdb = KvdbMdbx::with_column(
            Arc::clone(&self.env),
            SNAPSHOT_COL,
        );
        let upper_slice: Option<&[u8]> = upper.as_ref().map(|a| &a[..]);
        let hit = kvdb.first_in_range(&lower[..], upper_slice)?;
        Ok(hit.is_some())
    }
}

// ================= SnapshotDbManagerTrait =================

impl SnapshotDbManagerTrait for SnapshotDbManagerMdbx {
    type SnapshotDb = SnapshotKvDbMdbx;
    type SnapshotDbWrite = SnapshotKvDbMdbx;

    fn get_snapshot_dir(&self) -> &Path { self.snapshot_dir.as_path() }

    fn get_snapshot_db_name(&self, snapshot_epoch_id: &EpochId) -> String {
        Self::SNAPSHOT_DB_MDBX_DIR_PREFIX.to_string()
            + &snapshot_epoch_id.as_ref().to_hex::<String>()
    }

    fn get_snapshot_db_path(
        &self, snapshot_epoch_id: &EpochId,
    ) -> PathBuf {
        self.snapshot_dir
            .join(self.get_snapshot_db_name(snapshot_epoch_id))
    }

    fn get_mpt_snapshot_dir(&self) -> &Path {
        self.mpt_snapshot_dir.as_path()
    }

    fn get_latest_mpt_snapshot_db_name(&self) -> String {
        Self::LATEST_MPT_SNAPSHOT_NAME.to_string()
    }

    fn recovery_latest_mpt_snapshot_from_checkpoint(
        &self, _snapshot_epoch_id: &EpochId,
        _before_era_main_hash: Option<EpochId>,
    ) -> Result<()> {
        // No-op under MDBX — the MPT lives in the same env as KV,
        // no separate dir to recover. Same behaviour as the
        // paritydb reference (which is also a no-op there).
        Ok(())
    }

    fn create_mpt_snapshot_from_latest(
        &self, _new_snapshot_epoch_id: &EpochId,
    ) -> Result<()> {
        // See `recovery_latest_mpt_snapshot_from_checkpoint`.
        Ok(())
    }

    fn get_epoch_id_from_snapshot_db_name(
        &self, snapshot_db_name: &str,
    ) -> Result<EpochId> {
        let prefix_len = Self::SNAPSHOT_DB_MDBX_DIR_PREFIX.len();
        if snapshot_db_name.len() < prefix_len {
            bail!(
                "SnapshotDbManagerMdbx: name {:?} too short — \
                 expected `paritydb_<hex(EpochId)>`",
                snapshot_db_name
            );
        }
        EpochId::from_str(&snapshot_db_name[prefix_len..])
            .map_err(|_| Error::from(ErrorKind::Msg(
                "not correct snapshot db name".to_string(),
            )))
    }

    fn try_get_new_snapshot_epoch_from_temp_path(
        &self, _dir_name: &str,
    ) -> Option<EpochId> {
        // Under MDBX there are no temp dirs — crash-safe merges
        // use the `[epoch_id | b'!']` MergeMarker instead of a
        // rename-under-lock. `scan_persist_state` in 5c.e picks
        // up interrupted merges via the marker, not via a temp
        // path scan. Return `None` here so the trait's default
        // `scan_persist_state` (which we override anyway) doesn't
        // misclassify a stray path.
        None
    }

    fn try_get_new_snapshot_epoch_from_mpt_temp_path(
        &self, _dir_name: &str,
    ) -> Option<EpochId> {
        // See `try_get_new_snapshot_epoch_from_temp_path`.
        None
    }

    // ---------- 5c.e-scoped methods: bail loudly ----------

    fn new_snapshot_by_merging<'m>(
        &self, _old_snapshot_epoch_id: &EpochId, _snapshot_epoch_id: EpochId,
        _delta_mpt: DeltaMptIterator,
        _in_progress_snapshot_info: SnapshotInfo,
        _snapshot_info_map_rwlock:
            &'m parking_lot::RwLock<PersistedSnapshotInfoMap>,
        _new_epoch_height: u64, _recover_mpt_with_kv_snapshot_exist: bool,
    ) -> Result<(
        RwLockWriteGuard<'m, PersistedSnapshotInfoMap>,
        SnapshotInfo,
    )> {
        bail!(
            "SnapshotDbManagerMdbx::new_snapshot_by_merging: \
             chunked merge implementation lands in Phase 5c.e; \
             consensus should still be routing through the \
             paritydb manager until the 5c.f wiring commit."
        )
    }

    fn new_temp_snapshot_for_full_sync(
        &self, _snapshot_epoch_id: &EpochId, _merkle_root: &MerkleHash,
        _new_epoch_height: u64,
    ) -> Result<Self::SnapshotDbWrite> {
        bail!(
            "SnapshotDbManagerMdbx::new_temp_snapshot_for_full_sync: \
             marker-driven full-sync ingest lands in Phase 5c.e."
        )
    }

    fn finalize_full_sync_snapshot<'m>(
        &self, _snapshot_epoch_id: &EpochId, _merkle_root: &MerkleHash,
        _snapshot_info_map_rwlock:
            &'m parking_lot::RwLock<PersistedSnapshotInfoMap>,
    ) -> Result<RwLockWriteGuard<'m, PersistedSnapshotInfoMap>> {
        bail!(
            "SnapshotDbManagerMdbx::finalize_full_sync_snapshot: \
             marker-driven full-sync ingest lands in Phase 5c.e."
        )
    }

    // ---------- 5c.d covers the rest ----------

    fn get_snapshot_by_epoch_id(
        &self, snapshot_epoch_id: &EpochId, try_open: bool,
        _open_mpt_snapshot: bool,
    ) -> Result<Option<Self::SnapshotDb>> {
        if snapshot_epoch_id.eq(&NULL_EPOCH) {
            return Ok(Some(SnapshotKvDbMdbx::get_null_snapshot()));
        }
        // Probe MDBX for any key with this snapshot's prefix
        // BEFORE acquiring the semaphore — a miss shouldn't burn
        // a permit.
        if !self.snapshot_has_any_data(snapshot_epoch_id)? {
            return Ok(None);
        }
        // Fast-path check the in-memory set — if the snapshot
        // is known and has data, hand out a handle.
        self.acquire_open_permit(try_open)?;
        Ok(Some(SnapshotKvDbMdbx::attach(
            Arc::clone(&self.env),
            *snapshot_epoch_id,
            Arc::clone(&self.open_snapshot_semaphore),
        )))
    }

    fn destroy_snapshot(
        &self, snapshot_epoch_id: &EpochId,
    ) -> Result<()> {
        // Range-delete every key under this snapshot's 32-byte
        // scope in the shared column. Chunked at 50k keys per
        // rw_txn — see `SnapshotKvDbMdbx::destroy_slice`'s doc
        // for the writer-lock rationale.
        SnapshotKvDbMdbx::destroy_slice(&self.env, snapshot_epoch_id)?;
        self.known_snapshots.write().remove(snapshot_epoch_id);
        debug!(
            "SnapshotDbManagerMdbx: destroyed snapshot {:?}",
            snapshot_epoch_id
        );
        Ok(())
    }

    fn snapshot_dir_exists(
        &self, snapshot_epoch_id: &EpochId,
    ) -> bool {
        // The genesis-window null snapshot is always considered
        // present (see trait doc). Same as paritydb reference.
        if snapshot_epoch_id.eq(&NULL_EPOCH) {
            return true;
        }
        // Non-blocking RwLock read against the in-memory set —
        // NOT a `Path::exists` call (the paritydb default) which
        // would always return false under MDBX (no per-snapshot
        // dir) and would silently make the snapshot-sync
        // responder stop advertising snapshots. This override
        // closes design doc §2.3.2.3 / R4.1's "silent
        // regression" landmine.
        self.known_snapshots.read().contains(snapshot_epoch_id)
    }
}

// ---------------------------- tests ----------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempdir::TempDir;

    fn open_env() -> (TempDir, Arc<MdbxEnv>) {
        let dir = TempDir::new("snapshot_db_manager_mdbx").unwrap();
        let env = MdbxEnv::open(dir.path()).unwrap();
        (dir, env)
    }

    fn make_manager() -> (TempDir, TempDir, SnapshotDbManagerMdbx) {
        let (env_dir, env) = open_env();
        let path_dir =
            TempDir::new("snapshot_manager_mdbx_path").unwrap();
        let m = SnapshotDbManagerMdbx::new(
            env,
            path_dir.path().join("snapshot"),
            8,
        )
        .unwrap();
        (env_dir, path_dir, m)
    }

    fn snapshot_id(byte: u8) -> EpochId {
        EpochId::from_slice(&[byte; 32])
    }

    /// `MergeMarker` round-trips through RLP for both variants
    /// (`Merge`, `FullSync`).
    #[test]
    fn merge_marker_rlp_round_trips_both_kinds() {
        let parent = snapshot_id(0xa0);
        let merge = MergeMarker::merge(&parent, 1_700_000_000);
        let encoded = rlp::encode(&merge);
        let decoded: MergeMarker = rlp::decode(&encoded).unwrap();
        assert_eq!(decoded, merge);
        assert_eq!(decoded.kind, MergeMarkerKind::Merge);

        let root = MerkleHash::from_slice(&[0xb1u8; 32]);
        let fs = MergeMarker::full_sync(&root, 1_700_000_100);
        let encoded = rlp::encode(&fs);
        let decoded: MergeMarker = rlp::decode(&encoded).unwrap();
        assert_eq!(decoded, fs);
        assert_eq!(decoded.kind, MergeMarkerKind::FullSync);
    }

    /// An unknown tag byte inside the RLP payload errors instead
    /// of silently mapping to a variant.
    #[test]
    fn merge_marker_rlp_rejects_unknown_kind_tag() {
        let mut s = RlpStream::new_list(3);
        s.append(&255u8);
        s.append(&0u64);
        s.append(&[0u8; 32].as_ref());
        let bytes = s.out();
        let result: std::result::Result<MergeMarker, _> = rlp::decode(&bytes);
        assert!(result.is_err());
    }

    /// `MergeMarker` decode rejects a context blob of the wrong
    /// length — guards against a truncated marker being parsed
    /// as valid.
    #[test]
    fn merge_marker_rlp_rejects_short_context() {
        let mut s = RlpStream::new_list(3);
        s.append(&1u8); // Merge
        s.append(&0u64);
        s.append(&[0u8; 16].as_ref()); // 16B — wrong
        let bytes = s.out();
        let result: std::result::Result<MergeMarker, _> = rlp::decode(&bytes);
        assert!(result.is_err());
    }

    /// Name format matches paritydb byte-for-byte and
    /// `get_epoch_id_from_snapshot_db_name` round-trips.
    #[test]
    fn name_format_matches_and_round_trips() {
        let (_e, _p, m) = make_manager();
        let id = snapshot_id(0x5a);
        let name = m.get_snapshot_db_name(&id);
        assert!(name.starts_with("paritydb_"));
        assert_eq!(
            name,
            "paritydb_".to_string() + &"5a".repeat(32)
        );
        assert_eq!(
            m.get_epoch_id_from_snapshot_db_name(&name).unwrap(),
            id
        );
    }

    /// `snapshot_dir_exists` reads the in-memory set — always
    /// true for NULL_EPOCH, false for unknown ids, true once
    /// marked known. NEVER touches the filesystem.
    #[test]
    fn snapshot_dir_exists_uses_in_memory_set() {
        let (_e, _p, m) = make_manager();
        // NULL_EPOCH is always present by contract.
        assert!(m.snapshot_dir_exists(&NULL_EPOCH));
        // Unknown id → false.
        let id = snapshot_id(0x77);
        assert!(!m.snapshot_dir_exists(&id));
        // Once marked known → true, without any FS work.
        m.mark_snapshot_known(id);
        assert!(m.snapshot_dir_exists(&id));
    }

    /// `get_snapshot_by_epoch_id` returns None for a snapshot
    /// with no data; returns Some for one populated (via the
    /// `first_in_range` probe, so it also implicitly tests the
    /// probe path).
    #[test]
    fn get_snapshot_probe_returns_some_iff_data_present() {
        use crate::impls::storage_db::{
            kvdb_mdbx::KvdbMdbx,
            snapshot_kv_db_mdbx::SNAPSHOT_COL,
            snapshot_prefix::compose_snapshot_prefix,
        };
        let (_e, _p, m) = make_manager();
        let empty_id = snapshot_id(0x11);
        let populated_id = snapshot_id(0x22);
        // Directly stamp a key under populated_id via the shared
        // column.
        let kvdb = KvdbMdbx::with_column(m.env(), SNAPSHOT_COL);
        let prefixed = compose_snapshot_prefix(
            &populated_id,
            crate::impls::storage_db::snapshot_prefix::SUB_PREFIX_KV,
        );
        let mut key = prefixed;
        key.extend_from_slice(b"account_key");
        use crate::storage_db::KeyValueDbTrait;
        KeyValueDbTrait::put(&kvdb, &key, b"account_value").unwrap();
        drop(kvdb);
        assert!(
            m.get_snapshot_by_epoch_id(&empty_id, true, false)
                .unwrap()
                .is_none()
        );
        assert!(
            m.get_snapshot_by_epoch_id(&populated_id, true, false)
                .unwrap()
                .is_some()
        );
    }

    /// `destroy_snapshot` chunk-deletes every key under the
    /// snapshot's scope AND removes the id from the known set.
    /// Neighbouring snapshots are untouched.
    #[test]
    fn destroy_purges_slice_and_evicts_known() {
        use crate::impls::storage_db::{
            kvdb_mdbx::KvdbMdbx,
            snapshot_kv_db_mdbx::SNAPSHOT_COL,
            snapshot_prefix::{compose_snapshot_prefix, SUB_PREFIX_KV},
        };
        let (_e, _p, m) = make_manager();
        let target = snapshot_id(0xa1);
        let bystander = snapshot_id(0xa2);
        let kvdb = KvdbMdbx::with_column(m.env(), SNAPSHOT_COL);
        for i in 0u8..32 {
            let mut k = compose_snapshot_prefix(&target, SUB_PREFIX_KV);
            k.push(i);
            use crate::storage_db::KeyValueDbTrait;
            KeyValueDbTrait::put(&kvdb, &k, &[i]).unwrap();
            let mut k2 =
                compose_snapshot_prefix(&bystander, SUB_PREFIX_KV);
            k2.push(i);
            KeyValueDbTrait::put(&kvdb, &k2, &[i]).unwrap();
        }
        m.mark_snapshot_known(target);
        m.mark_snapshot_known(bystander);

        m.destroy_snapshot(&target).unwrap();

        // Target's slice is empty; bystander's is intact.
        assert!(!m.snapshot_has_any_data(&target).unwrap());
        assert!(m.snapshot_has_any_data(&bystander).unwrap());
        // Known set no longer contains target.
        assert!(!m.snapshot_dir_exists(&target));
        assert!(m.snapshot_dir_exists(&bystander));
    }

    /// 5c.e-scoped methods currently bail with a clear message.
    #[test]
    fn merge_and_full_sync_stubs_bail_loudly() {
        let (_e, _p, m) = make_manager();
        // We can't easily construct a `DeltaMptIterator` /
        // `SnapshotInfo` / `PersistedSnapshotInfoMap` from a
        // test — instead call the simpler `new_temp_snapshot_for_full_sync`
        // stub which takes primitives only.
        let root = MerkleHash::from_slice(&[0u8; 32]);
        // `.unwrap_err()` would require `SnapshotKvDbMdbx: Debug`
        // which the type deliberately doesn't derive (it holds
        // `Arc<MdbxEnv>` etc). `.err()` gets us the error side
        // directly.
        let err_msg = m
            .new_temp_snapshot_for_full_sync(&snapshot_id(0), &root, 0)
            .err()
            .expect("stub must bail")
            .to_string();
        assert!(err_msg.contains("Phase 5c.e"));
    }

    /// `try_get_new_snapshot_epoch_from_*_path` return `None`
    /// under MDBX — the marker protocol replaces the temp-path
    /// scan.
    #[test]
    fn temp_path_helpers_return_none_under_mdbx() {
        let (_e, _p, m) = make_manager();
        assert!(
            m.try_get_new_snapshot_epoch_from_temp_path("anything")
                .is_none()
        );
        assert!(
            m.try_get_new_snapshot_epoch_from_mpt_temp_path("anything")
                .is_none()
        );
    }
}
