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
use lazy_static::lazy_static;
use metrics::{register_meter_with_group, Counter, CounterUsize, Meter};
use parking_lot::{RwLock, RwLockWriteGuard};
use primitives::{EpochId, MerkleHash, NULL_EPOCH};
use rlp::{Decodable, DecoderError, Encodable, Rlp, RlpStream};
use rustc_hex::ToHex;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

// ---------------- 5c.e: aggregate-per-subsystem metrics ----------------
//
// Per design doc §6 Q#4 (option A confirmed by the user before 5c.a
// started): a fixed set of counters at `mdbx_snapshot.*` instead of
// the 4c per-snapshot pattern that leaks the metrics registry on
// long-lived archives. Bounded series count (well below dashboard
// throttling) — operators correlate to specific snapshot ids via
// log lines, not per-series labels.
lazy_static! {
    /// Cumulative successful `new_snapshot_by_merging` completions
    /// (the marker was written, the child data landed, and the
    /// snapshot_info registration committed under lock).
    static ref MDBX_SNAPSHOT_MERGES_OK: Arc<dyn Meter> =
        register_meter_with_group("mdbx_snapshot", "merges_ok_total");

    /// Cumulative errored `new_snapshot_by_merging` calls — the
    /// marker was written but something in the merge failed. The
    /// stale marker gets GC'd on the next `scan_persist_state`.
    static ref MDBX_SNAPSHOT_MERGES_FAIL: Arc<dyn Meter> =
        register_meter_with_group("mdbx_snapshot", "merges_fail_total");

    /// Cumulative full-sync ingests started (marker written) and
    /// finalised (marker deleted under lock).
    static ref MDBX_SNAPSHOT_FULLSYNC_STARTED: Arc<dyn Meter> =
        register_meter_with_group("mdbx_snapshot", "full_sync_started_total");
    static ref MDBX_SNAPSHOT_FULLSYNC_FINALIZED: Arc<dyn Meter> =
        register_meter_with_group("mdbx_snapshot", "full_sync_finalized_total");

    /// Cumulative `destroy_snapshot` invocations — either manager-
    /// driven (retention pruning, non-canonical-fork cleanup) or
    /// RAII-drop-driven (`remove_on_close`).
    static ref MDBX_SNAPSHOT_DESTROYS: Arc<dyn Meter> =
        register_meter_with_group("mdbx_snapshot", "destroys_total");

    /// Cumulative interrupted merges the recovery scan picked up
    /// via the marker sentinel. A steady-state value of 0 is
    /// expected; every crash-during-merge bumps this exactly once
    /// on the following restart.
    static ref MDBX_SNAPSHOT_RECOVERED_INTERRUPTED: Arc<dyn Meter> =
        register_meter_with_group("mdbx_snapshot", "recovered_interrupted_total");

    /// Cumulative orphan prefixes the recovery scan range-deleted
    /// (present in MDBX, not referenced by snapshot_info_map).
    /// Distinct from interrupted-merge recoveries above.
    static ref MDBX_SNAPSHOT_RECOVERED_ORPHANS: Arc<dyn Meter> =
        register_meter_with_group("mdbx_snapshot", "recovered_orphans_total");

    /// Gauge of the count of snapshots currently known to the
    /// manager (post-`scan_persist_state`). Republished on every
    /// `mark_snapshot_known` / `destroy_snapshot`.
    static ref MDBX_SNAPSHOT_KNOWN_COUNT: Arc<dyn metrics::Gauge<usize>> =
        metrics::GaugeUsize::register_with_group("mdbx_snapshot", "known_count");

    /// Aggregate per-op counters — cheaper than the 4c `<epoch_id>`
    /// series, but still tell operators when the snapshot tier is
    /// under load.
    static ref MDBX_SNAPSHOT_PUTS_OK: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group("mdbx_snapshot", "puts_ok_total");
    static ref MDBX_SNAPSHOT_GETS_OK: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group("mdbx_snapshot", "gets_ok_total");
    static ref MDBX_SNAPSHOT_GETS_MISS: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group("mdbx_snapshot", "gets_miss_total");
    static ref MDBX_SNAPSHOT_DELETES: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group("mdbx_snapshot", "deletes_total");
}

/// Current unix epoch seconds — kept separate from
/// `SystemTime::now` so tests can stub if needed. The value is
/// operator-facing only; correctness doesn't hinge on precise
/// clock values.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

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

    // ---------------- 5c.e: marker + prefix enumeration ----------------

    /// The marker key for `epoch_id` — `[epoch_id | b'!']`. Kept
    /// as a helper so both the merge writer and the recovery
    /// reader use the same layout.
    fn marker_key(epoch_id: &EpochId) -> Vec<u8> {
        use super::snapshot_prefix::{
            compose_snapshot_prefix, SUB_PREFIX_MERGE_MARKER,
        };
        // Zero-length "inner" key — the marker occupies exactly
        // the 33-byte prefix. Iteration on the sub-prefix returns
        // this one entry.
        compose_snapshot_prefix(epoch_id, SUB_PREFIX_MERGE_MARKER)
    }

    /// Write the crash-recovery marker for a merge-in-progress.
    /// Called by the FIRST rw_txn of `new_snapshot_by_merging`.
    /// Idempotent — MDBX overwrites the same key.
    fn write_marker(
        &self, epoch_id: &EpochId, marker: &MergeMarker,
    ) -> Result<()> {
        use super::{
            kvdb_mdbx::KvdbMdbx, snapshot_kv_db_mdbx::SNAPSHOT_COL,
        };
        use crate::storage_db::KeyValueDbTrait;
        let kvdb = KvdbMdbx::with_column(
            Arc::clone(&self.env),
            SNAPSHOT_COL,
        );
        let key = Self::marker_key(epoch_id);
        let value = rlp::encode(marker);
        KeyValueDbTrait::put(&kvdb, &key, &value)?;
        Ok(())
    }

    /// Delete the crash-recovery marker for a
    /// merge-just-committed snapshot. Called by the LAST rw_txn
    /// under the `snapshot_info_map_rwlock` write guard — this
    /// **is** the commit point of the marker protocol.
    fn delete_marker(&self, epoch_id: &EpochId) -> Result<()> {
        use super::{
            kvdb_mdbx::KvdbMdbx, snapshot_kv_db_mdbx::SNAPSHOT_COL,
        };
        use crate::storage_db::KeyValueDbTrait;
        let kvdb = KvdbMdbx::with_column(
            Arc::clone(&self.env),
            SNAPSHOT_COL,
        );
        let key = Self::marker_key(epoch_id);
        KeyValueDbTrait::delete(&kvdb, &key)?;
        Ok(())
    }

    /// Read the crash-recovery marker for a snapshot, if any.
    /// Consumed by `scan_persist_state` on the recovery path.
    fn read_marker(
        &self, epoch_id: &EpochId,
    ) -> Result<Option<MergeMarker>> {
        use super::{
            kvdb_mdbx::KvdbMdbx, snapshot_kv_db_mdbx::SNAPSHOT_COL,
        };
        let kvdb = KvdbMdbx::with_column(
            Arc::clone(&self.env),
            SNAPSHOT_COL,
        );
        let key = Self::marker_key(epoch_id);
        use crate::storage_db::KeyValueDbTraitRead;
        let raw = kvdb.get(&key)?;
        match raw {
            None => Ok(None),
            Some(bytes) => match rlp::decode::<MergeMarker>(&bytes) {
                Ok(m) => Ok(Some(m)),
                Err(e) => {
                    // Corrupt marker → warn + treat as "unknown
                    // stale prefix", GC without attempting resume.
                    warn!(
                        "SnapshotDbManagerMdbx: marker for {:?} is \
                         RLP-invalid ({}) — will be GC'd as an \
                         orphan on this scan_persist_state pass.",
                        epoch_id, e
                    );
                    Ok(None)
                }
            },
        }
    }

    /// Run the actual data-writing steps of a merge, after the
    /// marker has been written and before it is deleted. Split
    /// out of `new_snapshot_by_merging` so the marker-writing
    /// try/catch is easy to reason about. Any error here leaves
    /// the marker in place, which is exactly what we want —
    /// `scan_persist_state` will GC the partial child on the
    /// next boot.
    fn run_chunked_merge(
        &self, old_snapshot_epoch_id: &EpochId,
        snapshot_epoch_id: &EpochId, delta_mpt: DeltaMptIterator,
        recover_mpt_with_kv_snapshot_exist: bool,
        in_reconstruct_snapshot_state: bool,
    ) -> Result<MerkleHash> {
        // Attach a child handle. `attach` skips the semaphore —
        // production callers must not open a peer readonly view
        // of the child mid-merge (it wouldn't observe the marker
        // + dump anyway), so we don't need to burn a permit.
        let mut child = SnapshotKvDbMdbx::attach(
            Arc::clone(&self.env),
            *snapshot_epoch_id,
            Arc::clone(&self.open_snapshot_semaphore),
        );

        // Skip the delta dump when the caller is recovering the
        // MPT while the KV snapshot already exists — matches the
        // paritydb reference's early-out.
        if !recover_mpt_with_kv_snapshot_exist {
            child.dump_delta_mpt(&delta_mpt)?;
        }

        // Pick between direct_merge (no parent) and
        // copy_and_merge (parent → child). Same branch as
        // paritydb.
        let root = if *old_snapshot_epoch_id == NULL_EPOCH {
            child.direct_merge(
                None,
                &mut None,
                recover_mpt_with_kv_snapshot_exist,
                in_reconstruct_snapshot_state,
            )?
        } else {
            // Open the parent readonly. Errors here bubble out
            // (with the marker still in place, so the next boot's
            // scan_persist_state GCs the partial child).
            let parent = self
                .get_snapshot_by_epoch_id(
                    old_snapshot_epoch_id,
                    false,
                    false,
                )?
                .ok_or_else(|| Error::from(ErrorKind::SnapshotNotFound))?;
            let parent = Arc::new(parent);
            child.copy_and_merge(
                &parent,
                &mut None,
                in_reconstruct_snapshot_state,
            )?
        };
        Ok(root)
    }

    /// Enumerate every distinct 32-byte prefix present in the
    /// snapshot column. O(distinct_snapshots) cursor jumps —
    /// same trick as
    /// `DeltaDbManagerMdbx::enumerate_present_prefixes` per
    /// design doc §2.3.2.3.
    fn enumerate_present_prefixes(&self) -> Result<Vec<EpochId>> {
        use super::{
            kvdb_mdbx::KvdbMdbx, snapshot_kv_db_mdbx::SNAPSHOT_COL,
            snapshot_prefix::SNAPSHOT_EPOCH_ID_LEN,
        };
        let kvdb = KvdbMdbx::with_column(
            Arc::clone(&self.env),
            SNAPSHOT_COL,
        );
        let mut out = Vec::new();
        let mut cursor: Vec<u8> = Vec::new();
        loop {
            let hit = kvdb.first_in_range(&cursor[..], None)?;
            let (key, _val) = match hit {
                Some(kv) => kv,
                None => break,
            };
            if key.len() < SNAPSHOT_EPOCH_ID_LEN {
                cursor = key.to_vec();
                cursor.push(0);
                continue;
            }
            let mut prefix = [0u8; SNAPSHOT_EPOCH_ID_LEN];
            prefix.copy_from_slice(&key[..SNAPSHOT_EPOCH_ID_LEN]);
            let id = EpochId::from_slice(&prefix);
            out.push(id);
            // Jump past this snapshot's whole 32-byte scope.
            let mut next = prefix;
            let mut carry = true;
            for byte in next.iter_mut().rev() {
                if !carry {
                    break;
                }
                if *byte == 0xff {
                    *byte = 0;
                } else {
                    *byte += 1;
                    carry = false;
                }
            }
            if carry {
                break; // Was 0xff…ff — no more.
            }
            cursor = next.to_vec();
        }
        Ok(out)
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

    // ---------- 5c.e: chunked merge + marker protocol ----------

    /// Merge the delta MPT into a new child snapshot, staged
    /// crash-safely via the `[epoch_id | b'!']` marker key.
    ///
    /// Sequence (design doc §2.3.2.1):
    /// 1. Write `MergeMarker { Merge, started_at, parent }` at
    ///    the child's marker key. First rw_txn — declares
    ///    "there was a merge in flight for this epoch".
    /// 2. Open the child snapshot handle via `attach` (no disk
    ///    allocation — the prefix materialises on first write).
    /// 3. Dump the delta MPT into `SUB_PREFIX_DELTA_SET` /
    ///    `SUB_PREFIX_DELTA_DEL`.
    /// 4. Fold the delta into the MPT via `direct_merge` (no
    ///    parent) or `copy_and_merge` (parent → child).
    /// 5. Stamp the child's merkle root into
    ///    `in_progress_snapshot_info`.
    /// 6. Acquire the `snapshot_info_map_rwlock` write guard.
    /// 7. Delete the marker under that guard — the actual
    ///    commit point.
    /// 8. Register the child as known (in-memory set).
    /// 9. Update the `latest_snapshot_id` bookkeeping.
    /// 10. Return the guard so the caller can persist
    ///     `snapshot_info` under it — the analogue of
    ///     paritydb's "rename dir under the info lock".
    ///
    /// A crash between steps 1-6 leaves the marker present; a
    /// crash between 7 and 8 leaves the marker deleted but the
    /// snapshot unknown — `scan_persist_state` handles both
    /// cases (interrupted merge / orphan-without-info).
    fn new_snapshot_by_merging<'m>(
        &self, old_snapshot_epoch_id: &EpochId, snapshot_epoch_id: EpochId,
        delta_mpt: DeltaMptIterator,
        mut in_progress_snapshot_info: SnapshotInfo,
        snapshot_info_map_rwlock:
            &'m parking_lot::RwLock<PersistedSnapshotInfoMap>,
        _new_epoch_height: u64, recover_mpt_with_kv_snapshot_exist: bool,
    ) -> Result<(
        RwLockWriteGuard<'m, PersistedSnapshotInfoMap>,
        SnapshotInfo,
    )> {
        info!(
            "new_snapshot_by_merging (MDBX): old={:?} new={:?}",
            old_snapshot_epoch_id, snapshot_epoch_id
        );

        // Consensus can request a snapshot rebuild by staging the
        // target here — mirrors the paritydb reference.
        let in_reconstruct_snapshot_state = self
            .reconstruct_snapshot_id_for_reboot
            .write()
            .take()
            .is_some_and(|v| v == snapshot_epoch_id);

        // Step 1: write the marker (crash-recovery sentinel).
        let marker = MergeMarker::merge(
            old_snapshot_epoch_id,
            now_secs(),
        );
        if let Err(e) = self.write_marker(&snapshot_epoch_id, &marker) {
            MDBX_SNAPSHOT_MERGES_FAIL.mark(1);
            return Err(e);
        }

        // Steps 2-5: run the merge. Any error here leaves the
        // marker in place — the next `scan_persist_state` will
        // range-delete the partial child.
        let outcome = self.run_chunked_merge(
            old_snapshot_epoch_id,
            &snapshot_epoch_id,
            delta_mpt,
            recover_mpt_with_kv_snapshot_exist,
            in_reconstruct_snapshot_state,
        );
        let new_snapshot_root = match outcome {
            Ok(root) => root,
            Err(e) => {
                MDBX_SNAPSHOT_MERGES_FAIL.mark(1);
                // Do NOT delete the marker — `scan_persist_state`
                // needs it to GC the partial data.
                return Err(e);
            }
        };
        in_progress_snapshot_info.merkle_root = new_snapshot_root;

        // Steps 6-8: commit under the info-map lock.
        let locked = snapshot_info_map_rwlock.write();
        self.delete_marker(&snapshot_epoch_id)?;
        self.mark_snapshot_known(snapshot_epoch_id);
        // Bump the known-count gauge (aggregate metric).
        let known_len = self.known_snapshots.read().len();
        MDBX_SNAPSHOT_KNOWN_COUNT.update(known_len);

        MDBX_SNAPSHOT_MERGES_OK.mark(1);
        Ok((locked, in_progress_snapshot_info))
    }

    fn new_temp_snapshot_for_full_sync(
        &self, snapshot_epoch_id: &EpochId, merkle_root: &MerkleHash,
        _new_epoch_height: u64,
    ) -> Result<Self::SnapshotDbWrite> {
        // Write the full-sync marker BEFORE handing out the
        // handle. If the caller drops the handle without calling
        // `finalize_full_sync_snapshot`, the marker survives and
        // `scan_persist_state` GCs the partial state on the next
        // boot.
        let marker = MergeMarker::full_sync(merkle_root, now_secs());
        self.write_marker(snapshot_epoch_id, &marker)?;
        // Acquire a permit and hand out the handle.
        self.acquire_open_permit(false)?;
        MDBX_SNAPSHOT_FULLSYNC_STARTED.mark(1);
        Ok(SnapshotKvDbMdbx::attach(
            Arc::clone(&self.env),
            *snapshot_epoch_id,
            Arc::clone(&self.open_snapshot_semaphore),
        ))
    }

    fn finalize_full_sync_snapshot<'m>(
        &self, snapshot_epoch_id: &EpochId, _merkle_root: &MerkleHash,
        snapshot_info_map_rwlock:
            &'m parking_lot::RwLock<PersistedSnapshotInfoMap>,
    ) -> Result<RwLockWriteGuard<'m, PersistedSnapshotInfoMap>> {
        // Acquire the info-map lock, then delete the marker and
        // register the snapshot as known — the analogue of the
        // paritydb rename-under-lock.
        let locked = snapshot_info_map_rwlock.write();
        self.delete_marker(snapshot_epoch_id)?;
        self.mark_snapshot_known(*snapshot_epoch_id);
        let known_len = self.known_snapshots.read().len();
        MDBX_SNAPSHOT_KNOWN_COUNT.update(known_len);
        MDBX_SNAPSHOT_FULLSYNC_FINALIZED.mark(1);
        Ok(locked)
    }

    /// Override the default `scan_persist_state`: enumerate every
    /// distinct prefix present in MDBX, correlate against
    /// `snapshot_info_map`, GC orphans and interrupted merges,
    /// and populate the same [`SnapshotPersistState`] the paritydb
    /// default would have produced.
    ///
    /// Runs at [`crate::impls::storage_manager::StorageManager::load_persist_state`]
    /// time. O(distinct_snapshots) cursor jumps + O(distinct_snapshots)
    /// marker reads — never a full-column scan.
    ///
    /// See design doc §2.3.2.3 for the required output shape and
    /// the crash-recovery semantics.
    fn scan_persist_state(
        &self, snapshot_info_map: &HashMap<EpochId, SnapshotInfo>,
    ) -> Result<crate::storage_db::SnapshotPersistState> {
        use crate::storage_db::{
            SnapshotKeptToProvideSyncStatus, SnapshotPersistState,
        };

        // Build the expected set from snapshot_info_map. Same
        // partition the paritydb default uses: snapshots NOT
        // marked `InfoOnly` must have on-disk data; those marked
        // `InfoOnly` are info-only survivors (their ancestor is
        // still around for sync).
        let mut expected_full: HashMap<EpochId, u64> = HashMap::new();
        let mut info_only: HashSet<EpochId> = HashSet::new();
        for (id, info) in snapshot_info_map {
            if info.snapshot_info_kept_to_provide_sync
                != SnapshotKeptToProvideSyncStatus::InfoOnly
            {
                expected_full.insert(*id, info.height);
            } else {
                info_only.insert(*id);
            }
        }

        let present = self.enumerate_present_prefixes()?;

        let mut temp_snapshot_db_existing: Option<EpochId> = None;
        let mut removed_snapshots: HashSet<EpochId> = HashSet::new();
        let mut survivors: HashSet<EpochId> = HashSet::new();
        let mut max_epoch_id = NULL_EPOCH;
        let mut max_epoch_height: u64 = 0;

        for id in &present {
            let marker = self.read_marker(id)?;
            match marker {
                Some(m) => {
                    // Interrupted merge or full-sync — range-
                    // delete the partial child.
                    info!(
                        "scan_persist_state: recovering \
                         interrupted {:?} for snapshot {:?} \
                         (started_at={}); range-deleting partial \
                         data.",
                        m.kind, id, m.started_at
                    );
                    SnapshotKvDbMdbx::destroy_slice(&self.env, id)?;
                    MDBX_SNAPSHOT_RECOVERED_INTERRUPTED.mark(1);
                    if temp_snapshot_db_existing.is_none() {
                        temp_snapshot_db_existing = Some(*id);
                    } else {
                        // The design doc + paritydb reference
                        // both assume at most one temp at a time.
                        // Log loudly if two exist — likely a bug
                        // in the merge caller, but proceed with
                        // GC.
                        warn!(
                            "scan_persist_state: more than one \
                             in-progress marker present (already \
                             saw {:?}, now {:?}) — GC'ing both.",
                            temp_snapshot_db_existing.as_ref().unwrap(),
                            id
                        );
                    }
                }
                None => {
                    if expected_full.contains_key(id) {
                        survivors.insert(*id);
                        let h = expected_full[id];
                        if h > max_epoch_height {
                            max_epoch_height = h;
                            max_epoch_id = *id;
                        }
                    } else if info_only.contains(id) {
                        // Info-only snapshots have data but no
                        // full-tracking; leave them alone.
                        survivors.insert(*id);
                    } else {
                        // Orphan — present in MDBX but no
                        // snapshot_info entry.
                        info!(
                            "scan_persist_state: orphan prefix {:?} \
                             (no snapshot_info reference); \
                             range-deleting.",
                            id
                        );
                        SnapshotKvDbMdbx::destroy_slice(&self.env, id)?;
                        MDBX_SNAPSHOT_RECOVERED_ORPHANS.mark(1);
                        removed_snapshots.insert(*id);
                    }
                }
            }
        }

        // Any expected snapshot that has no MDBX data is
        // reported as missing (unless InfoOnly, which the
        // default's exclude-from-expected already handled).
        let mut missing_snapshots: Vec<EpochId> = Vec::new();
        for id in expected_full.keys() {
            if !survivors.contains(id) {
                missing_snapshots.push(*id);
            }
        }

        // Sync the in-memory known-snapshots set to survivors —
        // this is what `snapshot_dir_exists` reads.
        {
            let mut known = self.known_snapshots.write();
            *known = survivors.clone();
            MDBX_SNAPSHOT_KNOWN_COUNT.update(known.len());
        }
        // Stamp the latest known snapshot for bookkeeping.
        *self.latest_snapshot_id.write() =
            (max_epoch_id, max_epoch_height);

        // Under MDBX every snapshot always has its MPT in the
        // current db (§3 / R1.3). If ANY expected snapshot
        // survived, the max height above IS the max height with
        // MPT.
        let max_snapshot_epoch_height_has_mpt =
            if max_epoch_height > 0 {
                Some(max_epoch_height)
            } else {
                None
            };

        info!(
            "SnapshotDbManagerMdbx::scan_persist_state: max epoch \
             height {} (id {:?}), temp existing {:?}, removed {} \
             orphan(s), missing {} snapshot(s), max height with \
             MPT {:?}",
            max_epoch_height,
            max_epoch_id,
            temp_snapshot_db_existing,
            removed_snapshots.len(),
            missing_snapshots.len(),
            max_snapshot_epoch_height_has_mpt,
        );

        Ok(SnapshotPersistState {
            missing_snapshots,
            max_epoch_id,
            max_epoch_height,
            temp_snapshot_db_existing,
            removed_snapshots,
            max_snapshot_epoch_height_has_mpt,
        })
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
        // Republish the known-count gauge + bump aggregate meter.
        let known_len = self.known_snapshots.read().len();
        MDBX_SNAPSHOT_KNOWN_COUNT.update(known_len);
        MDBX_SNAPSHOT_DESTROYS.mark(1);
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

    // ---------------- 5c.e: marker + scan_persist_state ----------------
    //
    // Full merge/copy paths need real DeltaMptIterators + a
    // Persisted­SnapshotInfoMap; those are covered end-to-end in the
    // 5c.f integration test. Here we cover the marker primitives,
    // the full-sync lifecycle, and scan_persist_state's recovery
    // paths in isolation.

    /// The marker key for `epoch_id` is exactly the 33-byte
    /// prefix `[epoch_id | b'!']`.
    #[test]
    fn marker_key_layout() {
        use crate::impls::storage_db::snapshot_prefix::{
            SNAPSHOT_EPOCH_ID_LEN, SUB_PREFIX_MERGE_MARKER,
        };
        let id = snapshot_id(0x99);
        let key = SnapshotDbManagerMdbx::marker_key(&id);
        assert_eq!(key.len(), SNAPSHOT_EPOCH_ID_LEN + 1);
        assert_eq!(&key[..SNAPSHOT_EPOCH_ID_LEN], id.as_ref());
        assert_eq!(key[SNAPSHOT_EPOCH_ID_LEN], SUB_PREFIX_MERGE_MARKER);
    }

    /// Write → read → delete round-trip through the manager's
    /// marker primitives.
    #[test]
    fn write_read_delete_marker_round_trip() {
        let (_e, _p, m) = make_manager();
        let id = snapshot_id(0xdd);
        // Nothing there before.
        assert!(m.read_marker(&id).unwrap().is_none());
        let marker = MergeMarker::merge(&snapshot_id(0xd0), 42);
        m.write_marker(&id, &marker).unwrap();
        let read = m.read_marker(&id).unwrap().unwrap();
        assert_eq!(read, marker);
        m.delete_marker(&id).unwrap();
        assert!(m.read_marker(&id).unwrap().is_none());
    }

    /// A corrupt marker (arbitrary bytes at the marker key)
    /// decodes as `None` with a warn — recovery still range-
    /// deletes the prefix as an orphan.
    #[test]
    fn corrupt_marker_reads_as_none() {
        use crate::impls::storage_db::{
            kvdb_mdbx::KvdbMdbx, snapshot_kv_db_mdbx::SNAPSHOT_COL,
        };
        use crate::storage_db::KeyValueDbTrait;
        let (_e, _p, m) = make_manager();
        let id = snapshot_id(0xee);
        let kvdb = KvdbMdbx::with_column(m.env(), SNAPSHOT_COL);
        // Stamp garbage at the marker key.
        let key = SnapshotDbManagerMdbx::marker_key(&id);
        KeyValueDbTrait::put(&kvdb, &key, b"not-rlp").unwrap();
        // Reader returns None (garbage doesn't RLP-decode).
        assert!(m.read_marker(&id).unwrap().is_none());
    }

    /// `enumerate_present_prefixes` returns exactly the distinct
    /// 32-byte snapshot ids present in the column — same shape as
    /// `DeltaDbManagerMdbx`'s equivalent.
    #[test]
    fn enumerate_present_prefixes_all_snapshots() {
        use crate::impls::storage_db::{
            kvdb_mdbx::KvdbMdbx, snapshot_kv_db_mdbx::SNAPSHOT_COL,
            snapshot_prefix::{compose_snapshot_prefix, SUB_PREFIX_KV},
        };
        use crate::storage_db::KeyValueDbTrait;
        let (_e, _p, m) = make_manager();
        let ids: Vec<EpochId> =
            (0..4).map(|i| snapshot_id(0x10 + i)).collect();
        let kvdb = KvdbMdbx::with_column(m.env(), SNAPSHOT_COL);
        for id in &ids {
            let mut key = compose_snapshot_prefix(id, SUB_PREFIX_KV);
            key.extend_from_slice(b"payload");
            KeyValueDbTrait::put(&kvdb, &key, b"v").unwrap();
        }
        let mut found = m.enumerate_present_prefixes().unwrap();
        found.sort();
        let mut expected = ids;
        expected.sort();
        assert_eq!(found, expected);
    }

    /// Interrupted merge: `scan_persist_state` finds the marker,
    /// range-deletes the partial child, and reports it in
    /// `temp_snapshot_db_existing`.
    #[test]
    fn scan_persist_state_recovers_interrupted_merge() {
        use crate::impls::storage_db::{
            kvdb_mdbx::KvdbMdbx, snapshot_kv_db_mdbx::SNAPSHOT_COL,
            snapshot_prefix::{compose_snapshot_prefix, SUB_PREFIX_KV},
        };
        use crate::storage_db::KeyValueDbTrait;
        let (_e, _p, m) = make_manager();

        // Simulate the state of a mid-merge crash: marker present
        // + some partial child data.
        let child_id = snapshot_id(0x55);
        m.write_marker(
            &child_id,
            &MergeMarker::merge(&snapshot_id(0x54), 100),
        )
        .unwrap();
        let kvdb = KvdbMdbx::with_column(m.env(), SNAPSHOT_COL);
        let mut key =
            compose_snapshot_prefix(&child_id, SUB_PREFIX_KV);
        key.extend_from_slice(b"partial");
        KeyValueDbTrait::put(&kvdb, &key, b"data").unwrap();
        assert!(m.snapshot_has_any_data(&child_id).unwrap());

        // Nothing in snapshot_info_map — the child is unregistered.
        let empty: HashMap<EpochId, SnapshotInfo> = HashMap::new();
        let state = m.scan_persist_state(&empty).unwrap();
        assert_eq!(state.temp_snapshot_db_existing, Some(child_id));
        // Partial data purged.
        assert!(!m.snapshot_has_any_data(&child_id).unwrap());
        assert!(m.read_marker(&child_id).unwrap().is_none());
    }

    /// Orphan recovery: `scan_persist_state` range-deletes a
    /// prefix present in MDBX with no snapshot_info entry AND no
    /// marker.
    #[test]
    fn scan_persist_state_gcs_orphan_without_marker() {
        use crate::impls::storage_db::{
            kvdb_mdbx::KvdbMdbx, snapshot_kv_db_mdbx::SNAPSHOT_COL,
            snapshot_prefix::{compose_snapshot_prefix, SUB_PREFIX_KV},
        };
        use crate::storage_db::KeyValueDbTrait;
        let (_e, _p, m) = make_manager();
        let orphan_id = snapshot_id(0x66);
        let kvdb = KvdbMdbx::with_column(m.env(), SNAPSHOT_COL);
        let mut key = compose_snapshot_prefix(&orphan_id, SUB_PREFIX_KV);
        key.extend_from_slice(b"leftover");
        KeyValueDbTrait::put(&kvdb, &key, b"v").unwrap();

        let empty: HashMap<EpochId, SnapshotInfo> = HashMap::new();
        let state = m.scan_persist_state(&empty).unwrap();
        assert!(state.removed_snapshots.contains(&orphan_id));
        assert_eq!(state.temp_snapshot_db_existing, None);
        assert!(!m.snapshot_has_any_data(&orphan_id).unwrap());
    }

    /// Happy path: an expected snapshot present in both MDBX and
    /// snapshot_info_map is preserved, marks the max height, and
    /// gets into the in-memory known set.
    #[test]
    fn scan_persist_state_preserves_expected_snapshots() {
        use crate::impls::storage_db::{
            kvdb_mdbx::KvdbMdbx, snapshot_kv_db_mdbx::SNAPSHOT_COL,
            snapshot_prefix::{compose_snapshot_prefix, SUB_PREFIX_KV},
        };
        use crate::storage_db::{
            KeyValueDbTrait, SnapshotKeptToProvideSyncStatus,
        };
        let (_e, _p, m) = make_manager();
        let expected_id = snapshot_id(0xaa);
        let kvdb = KvdbMdbx::with_column(m.env(), SNAPSHOT_COL);
        let mut key =
            compose_snapshot_prefix(&expected_id, SUB_PREFIX_KV);
        key.extend_from_slice(b"account");
        KeyValueDbTrait::put(&kvdb, &key, b"balance").unwrap();

        let mut info_map: HashMap<EpochId, SnapshotInfo> = HashMap::new();
        let mut info = SnapshotInfo::genesis_snapshot_info();
        info.height = 42;
        info.parent_snapshot_epoch_id = expected_id;
        info.snapshot_info_kept_to_provide_sync =
            SnapshotKeptToProvideSyncStatus::No;
        info_map.insert(expected_id, info);

        let state = m.scan_persist_state(&info_map).unwrap();
        assert_eq!(state.max_epoch_id, expected_id);
        assert_eq!(state.max_epoch_height, 42);
        assert!(state.missing_snapshots.is_empty());
        assert!(state.removed_snapshots.is_empty());
        assert_eq!(
            state.max_snapshot_epoch_height_has_mpt,
            Some(42)
        );
        // Known-snapshots set synced from the survivors.
        assert!(m.snapshot_dir_exists(&expected_id));
    }

    /// Expected snapshot with NO data in MDBX shows up as
    /// `missing_snapshots`.
    #[test]
    fn scan_persist_state_reports_missing_snapshots() {
        use crate::storage_db::SnapshotKeptToProvideSyncStatus;
        let (_e, _p, m) = make_manager();
        let missing_id = snapshot_id(0xbb);
        let mut info_map: HashMap<EpochId, SnapshotInfo> = HashMap::new();
        let mut info = SnapshotInfo::genesis_snapshot_info();
        info.height = 7;
        info.parent_snapshot_epoch_id = missing_id;
        info.snapshot_info_kept_to_provide_sync =
            SnapshotKeptToProvideSyncStatus::No;
        info_map.insert(missing_id, info);
        let state = m.scan_persist_state(&info_map).unwrap();
        assert_eq!(state.missing_snapshots, vec![missing_id]);
        assert_eq!(state.max_epoch_id, NULL_EPOCH);
        assert_eq!(state.max_epoch_height, 0);
    }

    /// `new_temp_snapshot_for_full_sync` writes the FullSync
    /// marker AND acquires a semaphore permit. Finalize is covered
    /// end-to-end in the 5c.f integration test where
    /// `PersistedSnapshotInfoMap::new` is reachable via the
    /// storage-manager wiring; here we validate the marker is
    /// stamped and the handle isn't yet known.
    #[test]
    fn full_sync_start_writes_marker_and_defers_known() {
        let (_e, _p, m) = make_manager();
        let id = snapshot_id(0xcc);
        let root = MerkleHash::from_slice(&[0xcc; 32]);
        let handle = m
            .new_temp_snapshot_for_full_sync(&id, &root, 100)
            .unwrap();
        // Marker written under the FullSync kind, carrying the
        // expected root as context.
        let marker = m.read_marker(&id).unwrap().unwrap();
        assert_eq!(marker.kind, MergeMarkerKind::FullSync);
        assert_eq!(&marker.context[..], root.as_ref());
        // Not yet in the known set — finalize hasn't run.
        assert!(!m.snapshot_dir_exists(&id));
        drop(handle);
    }
}
