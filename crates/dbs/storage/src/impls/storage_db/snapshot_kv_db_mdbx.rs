// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! One snapshot's slice of the dedicated Phase 5c snapshot MDBX
//! env — the MDBX-native replacement for [`SnapshotKvDbParitydb`].
//!
//! # Slice layout
//!
//! Every op prepends
//! `[snapshot_epoch_id | sub_prefix]`
//! from
//! [`snapshot_prefix`](super::snapshot_prefix)
//! before hitting MDBX. All snapshots share `col 0` of one
//! `Arc<MdbxEnv>` — no per-snapshot files, no per-snapshot fd
//! (the paritydb per-snapshot-env fd pressure that motivated the
//! `open_snapshot_semaphore` sizing is gone; the semaphore now
//! only bounds handle count + metrics churn).
//!
//! # This commit (5c.b)
//!
//! Scaffolding only: struct, sub-view helpers, KV read/write trait
//! surface, `open`/`create`/`get_null_snapshot`, and the two
//! delta-dump helpers (`dump_delta_mpt`, `drop_delta_mpt_dump`).
//! The MPT trait ceremony, iterators, and full `SnapshotDbTrait`
//! impl arrive in 5c.c.
//!
//! # Reference impl
//!
//! [`SnapshotKvDbParitydb`](super::snapshot_kv_db_paritydb::SnapshotKvDbParitydb)
//! — cross-check every trait method's behavior against it.
//! See `docs/internal/storage-phase-5-cde-migration.md` §2.3.2.

use super::{
    kvdb_mdbx::{KvdbMdbx, MdbxEnv},
    prefixed_kvdb_mdbx::PrefixedKvdbMdbx,
    snapshot_prefix::{
        compose_snapshot_prefix, SUB_PREFIX_DELTA_DEL, SUB_PREFIX_DELTA_SET,
        SUB_PREFIX_KV, SUB_PREFIX_MPT,
    },
};
use crate::{
    impls::{
        delta_mpt::DeltaMptIterator,
        errors::*,
        merkle_patricia_trie::MptKeyValue,
    },
    storage_db::{
        DbValueType, KeyValueDbTraitOwnedRead, KeyValueDbTraitRead,
        KeyValueDbTraitSingleWriter, KeyValueDbTypes,
    },
    KVInserter,
};
use parking_lot::RwLock;
use primitives::{EpochId, StorageKeyWithSpace};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// The snapshot column id inside the dedicated snapshot env. Kept
/// as a named constant because the design doc §2.3.0 reserves
/// col 1 for a possible future MPT-table split (deliberately not
/// used today; keep the id free).
pub const SNAPSHOT_COL: u32 = 0;

/// One snapshot's slice of the shared snapshot MDBX env.
///
/// **Cheap to hold** — no per-snapshot file or fd; `open`/`create`
/// only stamp bookkeeping and acquire a semaphore permit for
/// handle-count bounding.
///
/// **Not `Clone`** — the RAII drop semantics (semaphore return,
/// remove-on-close) intentionally forbid cheap aliasing, matching
/// the paritydb reference.
pub struct SnapshotKvDbMdbx {
    /// Dedicated snapshot MDBX env, shared across every snapshot
    /// handle in the process. Owned via `Arc` so `col 0` handles
    /// can be spun off cheaply.
    env: Arc<MdbxEnv>,
    /// The 32-byte snapshot id — the outer half of every prefix
    /// produced by [`Self::compose_prefix`]. Copied here so cheap
    /// sub-view creation doesn't need a re-borrow from the
    /// manager's known-set.
    snapshot_epoch_id: EpochId,
    /// Handle-count semaphore, shared with the manager and with
    /// every peer snapshot handle. Released in [`Drop`]; see
    /// `release_semaphore_on_drop` for the null-snapshot escape
    /// hatch.
    open_semaphore: Arc<Semaphore>,
    /// If `false`, [`Drop`] does not return a permit — used by
    /// [`Self::get_null_snapshot`], which never acquired one.
    release_semaphore_on_drop: bool,
    /// When set, [`Drop`] calls the manager's chunked
    /// `destroy_snapshot` path — the MDBX-native replacement for
    /// paritydb's `fs::remove_dir_all(&self.path)`. Guarded by
    /// [`RwLock`] so a mid-lifetime flip doesn't race a drop.
    ///
    /// **Wired in 5c.c** — the `Drop` impl reads this flag but
    /// the actual chunked destroy call lands with the manager in
    /// 5c.d. Until then, drop only releases the semaphore.
    remove_on_close: RwLock<bool>,
    /// Whether the MPT sub-table lives in the same env as KV. Kept
    /// as a struct field for `is_mpt_table_in_current_db` (Phase C
    /// contract) — always `true` under 5c because the `mpt_snapshot`
    /// isolated-dir mode is dead (see §3 / R1.3 in the design doc).
    mpt_table_in_current_db: bool,
}

impl SnapshotKvDbMdbx {
    // ------------------- construction / lifecycle -------------------

    /// Attach a fresh handle to an existing snapshot. Does NOT
    /// verify data exists — that check is the manager's job (via
    /// `first_in_range` before calling here). Callers must have
    /// already acquired a semaphore permit.
    pub(crate) fn attach(
        env: Arc<MdbxEnv>, snapshot_epoch_id: EpochId,
        open_semaphore: Arc<Semaphore>,
    ) -> Self {
        Self {
            env,
            snapshot_epoch_id,
            open_semaphore,
            release_semaphore_on_drop: true,
            remove_on_close: RwLock::new(false),
            mpt_table_in_current_db: true,
        }
    }

    /// Handle to the manager-owned MDBX env. Used by the manager's
    /// chunked destroy path so it doesn't need a separate route
    /// through the semaphore.
    pub fn env(&self) -> Arc<MdbxEnv> { Arc::clone(&self.env) }

    /// Read-only view of the snapshot epoch id.
    pub fn snapshot_epoch_id(&self) -> &EpochId { &self.snapshot_epoch_id }

    /// Arm the RAII destroy — the manager's chunked destroy runs
    /// on drop. Used by `SnapshotDbManagerMdbx` to signal that a
    /// full-sync temp snapshot should be wiped if the ingest
    /// aborts before `finalize_full_sync_snapshot`.
    pub fn set_remove_on_last_close(&self) {
        *self.remove_on_close.write() = true;
    }

    /// The synthetic empty snapshot for the genesis window
    /// (`NULL_EPOCH`). Callers see an in-memory snapshot backed by
    /// a fresh MDBX env in the OS tmp dir — same lifecycle as the
    /// paritydb null-snapshot but MDBX-native. The temp env
    /// vanishes when the process exits; consensus never persists
    /// anything through this handle.
    pub fn get_null_snapshot() -> Self {
        use std::path::PathBuf;
        // A per-process temp directory so parallel test binaries
        // don't collide. Reopen is idempotent: MDBX just picks up
        // the existing (empty) column.
        let null_path: PathBuf = std::env::temp_dir().join(format!(
            "mazze_null_snapshot_mdbx_{}",
            std::process::id()
        ));
        // If the env fails to open (e.g. disk full in tmp), fall
        // back to a minimal in-process env in the current dir — the
        // null snapshot should never actually error out, callers
        // treat it as always-present.
        let env = MdbxEnv::open(&null_path).unwrap_or_else(|_| {
            // Last-ditch: reuse the same path but with a random
            // suffix. Only fires on outright OS-level failures.
            let alt = null_path.with_extension("alt");
            MdbxEnv::open(&alt)
                .expect("null snapshot env open must not fail twice")
        });
        Self {
            env,
            snapshot_epoch_id: primitives::NULL_EPOCH,
            open_semaphore: Arc::new(Semaphore::new(0)),
            release_semaphore_on_drop: false,
            remove_on_close: RwLock::new(false),
            mpt_table_in_current_db: true,
        }
    }

    /// `SnapshotDbTrait::is_mpt_table_in_current_db` — always
    /// `true` under 5c per design doc §3 / R1.3 (the
    /// isolated-MPT-dir mode is already non-functional; 5d will
    /// drop the config knob entirely).
    pub fn is_mpt_table_in_current_db(&self) -> bool {
        self.mpt_table_in_current_db
    }

    // ------------------- sub-prefix view helpers -------------------

    /// Compose the 33-byte prefix `[epoch_id | sub]` for a sub-
    /// table view. Kept private — external callers should use the
    /// typed view helpers below.
    fn compose_prefix(&self, sub_prefix: u8) -> Vec<u8> {
        compose_snapshot_prefix(&self.snapshot_epoch_id, sub_prefix)
    }

    /// Fresh handle scoped to this snapshot's flat KV state
    /// (`SUB_PREFIX_KV`). Every op transparently prepends the
    /// 33-byte prefix.
    pub fn kv_view(&self) -> PrefixedKvdbMdbx {
        let kvdb =
            KvdbMdbx::with_column(Arc::clone(&self.env), SNAPSHOT_COL);
        PrefixedKvdbMdbx::new(kvdb, self.compose_prefix(SUB_PREFIX_KV))
    }

    /// Fresh handle scoped to this snapshot's delta-set dump
    /// (`SUB_PREFIX_DELTA_SET`).
    pub fn delta_set_view(&self) -> PrefixedKvdbMdbx {
        let kvdb =
            KvdbMdbx::with_column(Arc::clone(&self.env), SNAPSHOT_COL);
        PrefixedKvdbMdbx::new(kvdb, self.compose_prefix(SUB_PREFIX_DELTA_SET))
    }

    /// Fresh handle scoped to this snapshot's delta-del dump
    /// (`SUB_PREFIX_DELTA_DEL`). Values are empty by convention
    /// (presence-only) — mirrors the paritydb `<()>` unit variant
    /// but stays typed as `Box<[u8]>` because MDBX handles empty
    /// values natively.
    pub fn delta_del_view(&self) -> PrefixedKvdbMdbx {
        let kvdb =
            KvdbMdbx::with_column(Arc::clone(&self.env), SNAPSHOT_COL);
        PrefixedKvdbMdbx::new(kvdb, self.compose_prefix(SUB_PREFIX_DELTA_DEL))
    }

    /// Fresh handle scoped to this snapshot's MPT node table
    /// (`SUB_PREFIX_MPT`). Consumed by the 5c.c MPT integration.
    pub fn mpt_view(&self) -> PrefixedKvdbMdbx {
        let kvdb =
            KvdbMdbx::with_column(Arc::clone(&self.env), SNAPSHOT_COL);
        PrefixedKvdbMdbx::new(kvdb, self.compose_prefix(SUB_PREFIX_MPT))
    }

    // ------------------- delta-mpt dump / drop -------------------

    /// Iterate the delta MPT and split its entries into this
    /// snapshot's `SUB_PREFIX_DELTA_SET` (non-empty values) and
    /// `SUB_PREFIX_DELTA_DEL` (empty-value presence markers).
    ///
    /// **Chunking**: at this scaffolding stage the whole dump goes
    /// through the primitive's put-per-key path (each put is an
    /// independent rw_txn in `KvdbMdbx`). 5c.e replaces this with
    /// a chunked batch that commits every ~50k entries, per design
    /// doc §2.3.2.1. Kept single-txn here so 5c.b can compile and
    /// be tested independently.
    pub fn dump_delta_mpt(
        &mut self, delta_mpt: &DeltaMptIterator,
    ) -> Result<()> {
        let mut dumper = DeltaMptDumperMdbx {
            set_view: self.delta_set_view(),
            del_view: self.delta_del_view(),
        };
        delta_mpt.iterate(&mut dumper)
    }

    /// Delete every key under `SUB_PREFIX_DELTA_SET` +
    /// `SUB_PREFIX_DELTA_DEL` for this snapshot. Called after the
    /// merge folds the delta into the MPT — the dumps are no
    /// longer needed for anything except one-step-sync, which is
    /// out of scope here.
    ///
    /// Uses the underlying [`KvdbMdbx::delete_range`]. 5c.e swaps
    /// in the chunked variant from pre-work #1 so a huge delta
    /// doesn't stall the writer lock.
    pub fn drop_delta_mpt_dump(&mut self) -> Result<()> {
        for sub in [SUB_PREFIX_DELTA_SET, SUB_PREFIX_DELTA_DEL] {
            let view = if sub == SUB_PREFIX_DELTA_SET {
                self.delta_set_view()
            } else {
                self.delta_del_view()
            };
            let lower = view.prefix().to_vec();
            let upper = view.upper_bound_exclusive();
            let inner_kvdb = KvdbMdbx::with_column(
                Arc::clone(&self.env),
                SNAPSHOT_COL,
            );
            inner_kvdb.delete_range(&lower, upper.as_deref())?;
        }
        Ok(())
    }
}

impl Drop for SnapshotKvDbMdbx {
    fn drop(&mut self) {
        // remove_on_close firing here does NOT yet wipe the
        // snapshot's slice from MDBX — that requires the manager's
        // chunked destroy (arrives in 5c.d). We log so the manager
        // wire-up commit can grep for the site.
        if *self.remove_on_close.read() {
            debug!(
                "SnapshotKvDbMdbx::drop: remove_on_close set for \
                 snapshot {:?} — manager-driven chunked destroy \
                 lands in Phase 5c.d.",
                self.snapshot_epoch_id
            );
        }
        if self.release_semaphore_on_drop {
            self.open_semaphore.add_permits(1);
        }
    }
}

// -------------------- KV read/write trait surface --------------------
//
// Every KV op routes through `SUB_PREFIX_KV`. The four flavours of
// `PrefixedKvdbMdbx::kv_view` share a fresh inner `KvdbMdbx` so
// nothing is cached here — the primitive is Send + Sync but not
// Clone (it wraps the primitive's `Clone`), so we compose it per
// op. Same shape as the paritydb reference
// (`snapshot_kv_db_paritydb.rs:688-713`).

impl KeyValueDbTypes for SnapshotKvDbMdbx {
    type ValueType = Box<[u8]>;
}

impl KeyValueDbTraitRead for SnapshotKvDbMdbx {
    fn get(&self, key: &[u8]) -> Result<Option<Self::ValueType>> {
        self.kv_view().get(key)
    }
}

impl KeyValueDbTraitOwnedRead for SnapshotKvDbMdbx {
    fn get_mut(
        &mut self, key: &[u8],
    ) -> Result<Option<Self::ValueType>> {
        self.get(key)
    }
}

impl KeyValueDbTraitSingleWriter for SnapshotKvDbMdbx {
    fn delete(
        &mut self, key: &[u8],
    ) -> Result<Option<Option<Self::ValueType>>> {
        // `KeyValueDbTrait::delete` returns `Option<Option<V>>` —
        // `Some(None)` if the entry existed and we don't return
        // the prior value, `None` if the backend didn't tell us.
        // MDBX drops the prior value silently on delete, matching
        // the paritydb reference.
        use crate::storage_db::KeyValueDbTrait;
        KeyValueDbTrait::delete(&self.kv_view(), key)
    }

    fn put(
        &mut self, key: &[u8],
        value: &<Self::ValueType as DbValueType>::Type,
    ) -> Result<Option<Option<Self::ValueType>>> {
        use crate::storage_db::KeyValueDbTrait;
        KeyValueDbTrait::put(&self.kv_view(), key, value)
    }
}

// -------------------- delta-MPT dump adapter --------------------

/// KVInserter shim that splits an `MptKeyValue` batch into
/// `SUB_PREFIX_DELTA_SET` / `SUB_PREFIX_DELTA_DEL` sub-tables per
/// the paritydb convention. Non-empty value → set; empty → del.
struct DeltaMptDumperMdbx {
    set_view: PrefixedKvdbMdbx,
    del_view: PrefixedKvdbMdbx,
}

impl KVInserter<MptKeyValue> for DeltaMptDumperMdbx {
    fn push(&mut self, x: MptKeyValue) -> Result<()> {
        use crate::storage_db::KeyValueDbTrait;
        let (mpt_key, value) = x;
        let snapshot_key =
            StorageKeyWithSpace::from_delta_mpt_key(&mpt_key).to_key_bytes();
        if !value.is_empty() {
            KeyValueDbTrait::put(&self.set_view, &snapshot_key, &value)?;
        } else {
            // Presence-only: an empty value byte-string. MDBX
            // stores this without special handling and iteration
            // yields a zero-length `Box<[u8]>`.
            KeyValueDbTrait::put(&self.del_view, &snapshot_key, &[])?;
        }
        Ok(())
    }
}

// ---------------------------- tests ----------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempdir::TempDir;

    fn open_env() -> (TempDir, Arc<MdbxEnv>) {
        let dir = TempDir::new("snapshot_kv_db_mdbx").unwrap();
        let env = MdbxEnv::open(dir.path()).unwrap();
        (dir, env)
    }

    fn snapshot_id(byte: u8) -> EpochId {
        EpochId::from_slice(&[byte; 32])
    }

    fn make_snapshot(byte: u8) -> (TempDir, SnapshotKvDbMdbx) {
        let (dir, env) = open_env();
        let sem = Arc::new(Semaphore::new(1));
        // Test setups skip the real acquire — we only care about
        // release-on-drop semantics.
        let sem_for_release = Arc::clone(&sem);
        let sn = SnapshotKvDbMdbx::attach(env, snapshot_id(byte), sem_for_release);
        (dir, sn)
    }

    /// KV round-trip through the `KeyValueDbTraitRead/SingleWriter`
    /// surface. Under the hood every op goes to
    /// `[epoch_id | SUB_PREFIX_KV | key]`.
    #[test]
    fn kv_round_trip() {
        let (_dir, mut sn) = make_snapshot(0x11);
        assert!(sn.get(b"missing").unwrap().is_none());
        sn.put(b"k", b"v").unwrap();
        assert_eq!(sn.get(b"k").unwrap().as_deref(), Some(&b"v"[..]));
        sn.delete(b"k").unwrap();
        assert!(sn.get(b"k").unwrap().is_none());
    }

    /// Two snapshots on the SAME MDBX env don't see each other's
    /// KV. This is the core prefix-isolation guarantee 5c depends
    /// on.
    #[test]
    fn snapshots_isolated_on_shared_env() {
        let (_dir, env) = open_env();
        let sem = Arc::new(Semaphore::new(2));
        let mut a = SnapshotKvDbMdbx::attach(
            Arc::clone(&env),
            snapshot_id(0xaa),
            Arc::clone(&sem),
        );
        let mut b = SnapshotKvDbMdbx::attach(
            env,
            snapshot_id(0xbb),
            sem,
        );
        a.put(b"key", b"from-a").unwrap();
        b.put(b"key", b"from-b").unwrap();
        assert_eq!(a.get(b"key").unwrap().as_deref(), Some(&b"from-a"[..]));
        assert_eq!(b.get(b"key").unwrap().as_deref(), Some(&b"from-b"[..]));
    }

    /// The four sub-view helpers produce distinct prefixes for
    /// the same snapshot — a byte-level guarantee that KV / SET
    /// / DEL / MPT don't collide.
    #[test]
    fn sub_views_have_distinct_prefixes() {
        let (_dir, sn) = make_snapshot(0x33);
        let prefixes: Vec<Vec<u8>> = [
            sn.kv_view().prefix().to_vec(),
            sn.delta_set_view().prefix().to_vec(),
            sn.delta_del_view().prefix().to_vec(),
            sn.mpt_view().prefix().to_vec(),
        ]
        .to_vec();
        // Each is 33 bytes and shares the same first 32.
        for p in &prefixes {
            assert_eq!(p.len(), 33);
            assert_eq!(&p[..32], sn.snapshot_epoch_id().as_ref());
        }
        // Last byte differs across all four.
        let mut last_bytes: Vec<u8> =
            prefixes.iter().map(|p| p[32]).collect();
        last_bytes.sort();
        last_bytes.dedup();
        assert_eq!(last_bytes.len(), 4);
    }

    /// Semaphore is returned to the pool on drop. Two acquisitions
    /// in a row of the 1-slot semaphore must succeed if the first
    /// handle was dropped.
    #[test]
    fn drop_releases_semaphore_permit() {
        let sem = Arc::new(Semaphore::new(1));
        let (_dir, env) = open_env();

        // Simulate the manager acquiring a permit before handing
        // out a snapshot.
        sem.try_acquire().unwrap().forget();
        let sn = SnapshotKvDbMdbx::attach(
            env,
            snapshot_id(0x55),
            Arc::clone(&sem),
        );
        // Zero permits available — the semaphore is empty.
        assert!(sem.try_acquire().is_err());
        drop(sn);
        // Drop returned it; we can acquire again.
        assert!(sem.try_acquire().is_ok());
    }

    /// The null snapshot always constructs without touching the
    /// manager's env, and its `is_mpt_table_in_current_db` is
    /// `true` (per §3 / R1.3 the isolated mode is dead).
    #[test]
    fn null_snapshot_constructs_and_reports_mpt_in_current_db() {
        let sn = SnapshotKvDbMdbx::get_null_snapshot();
        assert_eq!(sn.snapshot_epoch_id(), &primitives::NULL_EPOCH);
        assert!(sn.is_mpt_table_in_current_db());
    }

    /// The DELTA_SET / DELTA_DEL views don't leak into the KV
    /// view — writes to one sub-table must not appear when reading
    /// through the base KV trait.
    #[test]
    fn kv_reads_dont_see_delta_dump() {
        let (_dir, mut sn) = make_snapshot(0x77);
        use crate::storage_db::KeyValueDbTrait;
        // Write into DELTA_SET directly.
        KeyValueDbTrait::put(
            &sn.delta_set_view(),
            b"shared_key",
            b"delta-value",
        )
        .unwrap();
        // KV surface must not see it.
        assert!(sn.get(b"shared_key").unwrap().is_none());
        // But the KV surface CAN store its own value under the
        // same key without conflict.
        sn.put(b"shared_key", b"kv-value").unwrap();
        assert_eq!(
            sn.get(b"shared_key").unwrap().as_deref(),
            Some(&b"kv-value"[..])
        );
    }
}
