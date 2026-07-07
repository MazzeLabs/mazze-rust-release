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
    snapshot_mpt::{SnapshotMpt, SnapshotMptIterableDb, SnapshotMptLoadNode},
    snapshot_prefix::{
        compose_snapshot_prefix, SUB_PREFIX_DELTA_DEL, SUB_PREFIX_DELTA_SET,
        SUB_PREFIX_KV, SUB_PREFIX_MPT,
    },
};
use crate::{
    impls::{
        delta_mpt::DeltaMptIterator,
        errors::*,
        merkle_patricia_trie::{MptKeyValue, MptMerger},
    },
    storage_db::{
        key_value_db::KvdbIterIterator, DbValueType,
        KeyValueDbIterableTrait, KeyValueDbTraitOwnedRead,
        KeyValueDbTraitRead, KeyValueDbTraitSingleWriter, KeyValueDbTypes,
        OpenSnapshotMptTrait, SnapshotDbTrait, SnapshotDbWriteableTrait,
        SnapshotMptDbTrait, SnapshotMptDbValue,
        SnapshotMptTraitReadAndIterate, SnapshotMptTraitRw,
    },
    utils::{
        tuple::ElementSatisfy,
        wrap::{Wrap, WrappedLifetimeFamily, WrappedTrait},
    },
    KVInserter,
};
use fallible_iterator::FallibleIterator;
use parking_lot::RwLock;
use primitives::{EpochId, MerkleHash, StorageKeyWithSpace};
use std::{marker::PhantomData, sync::Arc};
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

// ============================================================
// 5c.c: iterator + MPT + SnapshotDbTrait ceremony
// ============================================================
//
// Everything above (5c.b) built the KV read/write scaffolding.
// Everything below (5c.c) adds the iterator surface, the MPT
// integration, and the full `SnapshotDbTrait` impl so
// `SnapshotDbManagerMdbx` (arriving in 5c.d) can hand out
// `SnapshotKvDbMdbx` handles as `Self::SnapshotDb` in the trait.
//
// Iterator design (per user's 5c.c option-b): materialize the
// whole prefix range into a `Vec` up front, then wrap it in a
// `FallibleIterator` that strips the 33-byte prefix and applies
// caller-supplied bounds. Simpler than paritydb's streaming
// cursor iter — trades ~O(range_size) allocation up front for
// ~150 LOC less trait plumbing. Fine here because every real
// consumer (`snapshot_kv_iterator`, delta-set dump replay,
// `direct_merge` / `copy_and_merge`) walks the range to
// completion anyway.

/// Tag marking the `PrefixedKvdbMdbx` iterator lineage — needed
/// for the `WrappedTrait` / `WrappedLifetimeFamily` dispatch that
/// makes `KeyValueDbIterableTrait<Item, KeyType, Tag>` compile
/// (the trait's `KvdbIterIterator<Item, KeyType, Tag>` associated
/// type carries the tag).
pub struct KvdbMdbxIteratorTag;

/// Typed empty byte slice for `iter_range`'s "no lower bound"
/// call sites — a bare `&[]` literal in an inference context
/// binds as `&[_; 0]` rather than `&[u8]`, which is enough to
/// break trait resolution when two `iter_range` variants (Item =
/// `MptKeyValue` vs `(Vec<u8>, ())`) are in scope. Passing this
/// binding keeps the KeyType unambiguous.
const EMPTY_KEY: &[u8] = &[];

/// Materialised-upfront range iterator over one snapshot sub-
/// table slice. Owns the `Vec` returned by
/// [`KvdbMdbx::iter_range_owned`], yielding one entry at a time
/// with the 33-byte snapshot prefix stripped and caller-supplied
/// bounds applied.
///
/// Generic over `Item` so we can yield both `MptKeyValue`
/// (`(Vec<u8>, Box<[u8]>)`) for the KV / SET / MPT slices and
/// `(Vec<u8>, ())` for the DEL slice; the paritydb reference
/// carries the same distinction via two typed `PrefixedKvdb`
/// wrappers.
pub struct MdbxRangeIter<Item> {
    /// The full-key/value pairs materialised up front. Consumed
    /// as we yield.
    remaining: std::vec::IntoIter<(Box<[u8]>, Box<[u8]>)>,
    /// Bytes to strip off the front of every key (33 = 32B
    /// epoch + 1B sub_prefix under the snapshot layout).
    prefix_len: usize,
    /// Caller-supplied lower bound on the *stripped* key. `None`
    /// means "no lower bound" (bytes empty on the input side).
    lower_bound: Option<Vec<u8>>,
    /// If `true`, keys equal to `lower_bound` are excluded. Set
    /// by `iter_range_excl`.
    lower_exclusive: bool,
    /// Caller-supplied exclusive upper bound on the stripped
    /// key. `None` means "no upper bound" — walk to the end of
    /// the slice.
    upper_bound: Option<Vec<u8>>,
    _marker: PhantomData<Item>,
}

impl<Item> MdbxRangeIter<Item> {
    /// Strip the snapshot prefix off a raw MDBX key. Returns
    /// `None` when the key doesn't start with the expected
    /// prefix — signals end-of-slice (the `iter_range_owned`
    /// upper bound should already prevent this, but defence in
    /// depth).
    fn strip_prefix<'k>(&self, key: &'k [u8]) -> Option<&'k [u8]> {
        if key.len() >= self.prefix_len {
            Some(&key[self.prefix_len..])
        } else {
            None
        }
    }

    /// Common bounds-check: `stripped` in `[lower, upper)` (with
    /// `lower_exclusive` flipping to `(lower, upper)`).
    fn in_bounds(&self, stripped: &[u8]) -> BoundsCheck {
        if let Some(lower) = &self.lower_bound {
            if self.lower_exclusive {
                if stripped <= lower.as_slice() {
                    return BoundsCheck::Skip;
                }
            } else if stripped < lower.as_slice() {
                return BoundsCheck::Skip;
            }
        }
        if let Some(upper) = &self.upper_bound {
            if stripped >= upper.as_slice() {
                return BoundsCheck::Stop;
            }
        }
        BoundsCheck::Emit
    }
}

/// Result of the per-entry bounds check inside `MdbxRangeIter::next`.
enum BoundsCheck {
    /// Yield this entry.
    Emit,
    /// Below the lower bound; skip and try the next raw entry.
    Skip,
    /// At or beyond the upper bound; iteration is done.
    Stop,
}

impl FallibleIterator for MdbxRangeIter<Box<[u8]>> {
    type Item = MptKeyValue;
    type Error = Error;

    fn next(&mut self) -> Result<Option<Self::Item>> {
        loop {
            let (key, value) = match self.remaining.next() {
                None => return Ok(None),
                Some(kv) => kv,
            };
            let stripped = match self.strip_prefix(&key) {
                None => return Ok(None),
                Some(k) => k,
            };
            match self.in_bounds(stripped) {
                BoundsCheck::Skip => continue,
                BoundsCheck::Stop => return Ok(None),
                BoundsCheck::Emit => {
                    return Ok(Some((stripped.to_vec(), value)));
                }
            }
        }
    }
}

impl FallibleIterator for MdbxRangeIter<()> {
    type Item = (Vec<u8>, ());
    type Error = Error;

    fn next(&mut self) -> Result<Option<Self::Item>> {
        loop {
            let (key, _value) = match self.remaining.next() {
                None => return Ok(None),
                Some(kv) => kv,
            };
            let stripped = match self.strip_prefix(&key) {
                None => return Ok(None),
                Some(k) => k,
            };
            match self.in_bounds(stripped) {
                BoundsCheck::Skip => continue,
                BoundsCheck::Stop => return Ok(None),
                BoundsCheck::Emit => return Ok(Some((stripped.to_vec(), ()))),
            }
        }
    }
}

// -------- WrappedTrait / WrappedLifetimeFamily dispatch --------
//
// Same shape as the paritydb reference — the concrete
// `MdbxRangeIter<T>` type is emitted as
// `<KvdbIterIterator<Item, [u8], KvdbMdbxIteratorTag> as
//   WrappedLifetimeFamily<'a, dyn FallibleIterator<Item=Item, Error=Error>>>::Out`.
// Two variants for the two `Item` types.

impl<'a>
    WrappedLifetimeFamily<
        'a,
        dyn FallibleIterator<Item = MptKeyValue, Error = Error>,
    > for KvdbIterIterator<MptKeyValue, [u8], KvdbMdbxIteratorTag>
{
    type Out = MdbxRangeIter<Box<[u8]>>;
}

impl WrappedTrait<dyn FallibleIterator<Item = MptKeyValue, Error = Error>>
    for KvdbIterIterator<MptKeyValue, [u8], KvdbMdbxIteratorTag>
{
}

impl<'a>
    WrappedLifetimeFamily<
        'a,
        dyn FallibleIterator<Item = (Vec<u8>, ()), Error = Error>,
    > for KvdbIterIterator<(Vec<u8>, ()), [u8], KvdbMdbxIteratorTag>
{
    type Out = MdbxRangeIter<()>;
}

impl WrappedTrait<dyn FallibleIterator<Item = (Vec<u8>, ()), Error = Error>>
    for KvdbIterIterator<(Vec<u8>, ()), [u8], KvdbMdbxIteratorTag>
{
}

// -------- KeyValueDbIterableTrait for PrefixedKvdbMdbx --------
//
// Two impl blocks — one for MptKeyValue, one for the unit
// variant — both wrap the same `iter_range_owned` call. Mirrors
// paritydb's two-typed-prefixed-kvdb pattern but off a single
// `PrefixedKvdbMdbx` type.

/// Concatenate `self.prefix() ++ user_key` for a range-endpoint.
/// Kept as a free function so both trait impls can call it
/// without duplicating the buffer setup.
fn compose_range_endpoint(
    prefixed: &PrefixedKvdbMdbx, inner_key: &[u8],
) -> Vec<u8> {
    let prefix = prefixed.prefix();
    let mut out = Vec::with_capacity(prefix.len() + inner_key.len());
    out.extend_from_slice(prefix);
    out.extend_from_slice(inner_key);
    out
}

/// The exclusive upper bound of `prefixed`'s slice — `prefix + 1`
/// as a byte-vector. Panics only if the prefix is `0xff…ff`, at
/// which point iteration into the slice can't be bounded and the
/// caller should have kept a shorter prefix.
fn snapshot_slice_upper_bound(prefixed: &PrefixedKvdbMdbx) -> Vec<u8> {
    prefixed.upper_bound_exclusive().unwrap_or_else(|| {
        // Astronomically improbable — every snapshot uses a
        // real epoch id so the prefix is never `0xff…ff`.
        panic!(
            "PrefixedKvdbMdbx: snapshot prefix has no representable \
             upper bound (would iterate off the end of the column)"
        )
    })
}

impl KeyValueDbIterableTrait<MptKeyValue, [u8], KvdbMdbxIteratorTag>
    for PrefixedKvdbMdbx
{
    fn iter_range(
        &mut self, lower_bound_incl: &[u8], upper_bound_excl: Option<&[u8]>,
    ) -> Result<
        Wrap<
            KvdbIterIterator<MptKeyValue, [u8], KvdbMdbxIteratorTag>,
            dyn FallibleIterator<Item = MptKeyValue, Error = Error>,
        >,
    > {
        let raw_lower = compose_range_endpoint(self, lower_bound_incl);
        let raw_upper_owned = match upper_bound_excl {
            Some(u) => compose_range_endpoint(self, u),
            None => snapshot_slice_upper_bound(self),
        };
        let items = self
            .inner_kvdb()
            .iter_range_owned(&raw_lower, Some(&raw_upper_owned))?;
        let lower_opt = if lower_bound_incl.is_empty() {
            None
        } else {
            Some(lower_bound_incl.to_vec())
        };
        let upper_opt = upper_bound_excl.map(|u| u.to_vec());
        Ok(Wrap(MdbxRangeIter {
            remaining: items.into_iter(),
            prefix_len: self.prefix().len(),
            lower_bound: lower_opt,
            lower_exclusive: false,
            upper_bound: upper_opt,
            _marker: PhantomData,
        }))
    }

    fn iter_range_excl(
        &mut self, lower_bound_excl: &[u8], upper_bound_excl: &[u8],
    ) -> Result<
        Wrap<
            KvdbIterIterator<MptKeyValue, [u8], KvdbMdbxIteratorTag>,
            dyn FallibleIterator<Item = MptKeyValue, Error = Error>,
        >,
    > {
        let raw_lower = compose_range_endpoint(self, lower_bound_excl);
        let raw_upper_owned = compose_range_endpoint(self, upper_bound_excl);
        let items = self
            .inner_kvdb()
            .iter_range_owned(&raw_lower, Some(&raw_upper_owned))?;
        let lower_opt = if lower_bound_excl.is_empty() {
            None
        } else {
            Some(lower_bound_excl.to_vec())
        };
        Ok(Wrap(MdbxRangeIter {
            remaining: items.into_iter(),
            prefix_len: self.prefix().len(),
            lower_bound: lower_opt,
            lower_exclusive: true,
            upper_bound: Some(upper_bound_excl.to_vec()),
            _marker: PhantomData,
        }))
    }
}

impl KeyValueDbIterableTrait<(Vec<u8>, ()), [u8], KvdbMdbxIteratorTag>
    for PrefixedKvdbMdbx
{
    fn iter_range(
        &mut self, lower_bound_incl: &[u8], upper_bound_excl: Option<&[u8]>,
    ) -> Result<
        Wrap<
            KvdbIterIterator<(Vec<u8>, ()), [u8], KvdbMdbxIteratorTag>,
            dyn FallibleIterator<Item = (Vec<u8>, ()), Error = Error>,
        >,
    > {
        let raw_lower = compose_range_endpoint(self, lower_bound_incl);
        let raw_upper_owned = match upper_bound_excl {
            Some(u) => compose_range_endpoint(self, u),
            None => snapshot_slice_upper_bound(self),
        };
        let items = self
            .inner_kvdb()
            .iter_range_owned(&raw_lower, Some(&raw_upper_owned))?;
        let lower_opt = if lower_bound_incl.is_empty() {
            None
        } else {
            Some(lower_bound_incl.to_vec())
        };
        let upper_opt = upper_bound_excl.map(|u| u.to_vec());
        Ok(Wrap(MdbxRangeIter {
            remaining: items.into_iter(),
            prefix_len: self.prefix().len(),
            lower_bound: lower_opt,
            lower_exclusive: false,
            upper_bound: upper_opt,
            _marker: PhantomData,
        }))
    }

    fn iter_range_excl(
        &mut self, lower_bound_excl: &[u8], upper_bound_excl: &[u8],
    ) -> Result<
        Wrap<
            KvdbIterIterator<(Vec<u8>, ()), [u8], KvdbMdbxIteratorTag>,
            dyn FallibleIterator<Item = (Vec<u8>, ()), Error = Error>,
        >,
    > {
        let raw_lower = compose_range_endpoint(self, lower_bound_excl);
        let raw_upper_owned = compose_range_endpoint(self, upper_bound_excl);
        let items = self
            .inner_kvdb()
            .iter_range_owned(&raw_lower, Some(&raw_upper_owned))?;
        let lower_opt = if lower_bound_excl.is_empty() {
            None
        } else {
            Some(lower_bound_excl.to_vec())
        };
        Ok(Wrap(MdbxRangeIter {
            remaining: items.into_iter(),
            prefix_len: self.prefix().len(),
            lower_bound: lower_opt,
            lower_exclusive: true,
            upper_bound: Some(upper_bound_excl.to_vec()),
            _marker: PhantomData,
        }))
    }
}

// -------- ElementSatisfy / Wrap for the SnapshotDbTrait's
// `SnapshotKvdbIterType` associated type --------
//
// `SnapshotDbTrait::SnapshotKvdbIterType: WrappedTrait<dyn
// KeyValueDbIterableTrait<MptKeyValue, [u8], IterTag>>` — we
// pick `PrefixedKvdbMdbx` as the iter type and satisfy the two
// wrapping traits so it can round-trip through the trait's
// `Wrap<...>` return.

impl
    ElementSatisfy<
        dyn KeyValueDbIterableTrait<MptKeyValue, [u8], KvdbMdbxIteratorTag>,
    > for PrefixedKvdbMdbx
{
    fn to_constrain_object(
        &self,
    ) -> &(dyn KeyValueDbIterableTrait<
        MptKeyValue,
        [u8],
        KvdbMdbxIteratorTag,
    > + 'static) {
        self
    }

    fn to_constrain_object_mut(
        &mut self,
    ) -> &mut (dyn KeyValueDbIterableTrait<
        MptKeyValue,
        [u8],
        KvdbMdbxIteratorTag,
    > + 'static) {
        self
    }
}

impl
    WrappedLifetimeFamily<
        '_,
        dyn KeyValueDbIterableTrait<MptKeyValue, [u8], KvdbMdbxIteratorTag>,
    > for PrefixedKvdbMdbx
{
    type Out = Self;
}

impl
    WrappedTrait<
        dyn KeyValueDbIterableTrait<MptKeyValue, [u8], KvdbMdbxIteratorTag>,
    > for PrefixedKvdbMdbx
{
}

// -------- MPT integration --------

impl SnapshotMptLoadNode for PrefixedKvdbMdbx {
    fn load_node_rlp(
        &mut self, key: &[u8],
    ) -> Result<Option<SnapshotMptDbValue>> {
        // `PrefixedKvdbMdbx::get` is impl'd on the plain type via
        // `KeyValueDbTraitRead`. `get_mut` would require the
        // `OwnedReadImplFamily` blanket which
        // `mark_kvdb_multi_reader!` only wires up for
        // `&PrefixedKvdbMdbx`; use the shared-borrow read path
        // directly. MDBX MVCC makes reads concurrent so there's
        // no semantic difference.
        <PrefixedKvdbMdbx as KeyValueDbTraitRead>::get(&*self, key)
    }
}

impl SnapshotMptIterableDb for PrefixedKvdbMdbx {
    type IterTag = KvdbMdbxIteratorTag;
}

// -------- SnapshotKvDbMdbx: iterator / MPT / trait extensions --------

impl SnapshotKvDbMdbx {
    /// Iterator over this snapshot's flat KV state
    /// (`SUB_PREFIX_KV`). Consumed by `copy_and_merge` to walk
    /// the parent snapshot's whole state into the child.
    pub fn snapshot_kv_iterator(
        &self,
    ) -> Result<
        Wrap<
            PrefixedKvdbMdbx,
            dyn KeyValueDbIterableTrait<
                MptKeyValue,
                [u8],
                KvdbMdbxIteratorTag,
            >,
        >,
    > {
        Ok(Wrap(self.kv_view()))
    }

    /// Iterator over this snapshot's MPT node table
    /// (`SUB_PREFIX_MPT`). Consumed by `direct_merge` when
    /// carrying MPT nodes forward from the parent snapshot into
    /// the child (which then applies the delta on top).
    pub fn snapshot_mpt_iterator(
        &self,
    ) -> Result<
        Wrap<
            PrefixedKvdbMdbx,
            dyn KeyValueDbIterableTrait<
                MptKeyValue,
                [u8],
                KvdbMdbxIteratorTag,
            >,
        >,
    > {
        Ok(Wrap(self.mpt_view()))
    }
}

// -------- SnapshotDbWriteableTrait --------
//
// Transactions are a no-op under MDBX in this scaffolding — every
// put/get lands as its own rw_txn on the primitive. 5c.e replaces
// this with the chunked writer that batches ~50k puts per rw_txn.
// `SnapshotDbWriteableTrait::start_transaction /
// commit_transaction` still exist for API-shape parity with the
// paritydb reference, but they don't stage anything.

impl SnapshotDbWriteableTrait for SnapshotKvDbMdbx {
    type SnapshotDbBorrowMutType =
        SnapshotMpt<PrefixedKvdbMdbx, PrefixedKvdbMdbx>;

    fn start_transaction(&mut self) -> Result<()> {
        // Placeholder — MDBX rw_txns are per-put. 5c.e installs
        // the chunked writer here.
        Ok(())
    }

    fn commit_transaction(&mut self) -> Result<()> {
        // See `start_transaction` — no-op today.
        Ok(())
    }

    fn put_kv(
        &mut self, key: &[u8],
        value: &<Self::ValueType as DbValueType>::Type,
    ) -> Result<Option<Option<Self::ValueType>>> {
        <Self as KeyValueDbTraitSingleWriter>::put(self, key, value)
    }

    fn open_snapshot_mpt_owned(
        &mut self,
    ) -> Result<Self::SnapshotDbBorrowMutType> {
        SnapshotMpt::new(self.mpt_view())
    }
}

// -------- SnapshotMptDbTrait --------

impl SnapshotMptDbTrait for SnapshotKvDbMdbx {
    fn start_transaction(&mut self) -> Result<()> {
        <Self as SnapshotDbTrait>::start_transaction(self)
    }

    fn commit_transaction(&mut self) -> Result<()> {
        <Self as SnapshotDbTrait>::commit_transaction(self)
    }
}

// -------- OpenSnapshotMptTrait --------
//
// Three lifetime variants — owned, borrow-mut, borrow-shared —
// all yield a fresh `SnapshotMpt` over the snapshot's
// `SUB_PREFIX_MPT` slice. MDBX MVCC gives readers concurrent
// access, so borrow-shared is safe from multiple threads.

impl<'db> OpenSnapshotMptTrait<'db> for SnapshotKvDbMdbx {
    type SnapshotDbAsOwnedType =
        SnapshotMpt<PrefixedKvdbMdbx, PrefixedKvdbMdbx>;
    type SnapshotDbBorrowMutType =
        SnapshotMpt<PrefixedKvdbMdbx, PrefixedKvdbMdbx>;
    type SnapshotDbBorrowSharedType =
        SnapshotMpt<PrefixedKvdbMdbx, PrefixedKvdbMdbx>;

    fn open_snapshot_mpt_owned(
        &'db mut self,
    ) -> Result<Self::SnapshotDbBorrowMutType> {
        SnapshotMpt::new(self.mpt_view())
    }

    fn open_snapshot_mpt_as_owned(
        &'db self,
    ) -> Result<Self::SnapshotDbAsOwnedType> {
        SnapshotMpt::new(self.mpt_view())
    }

    fn open_snapshot_mpt_shared(
        &'db self,
    ) -> Result<Self::SnapshotDbBorrowSharedType> {
        SnapshotMpt::new(self.mpt_view())
    }
}

// -------- SnapshotDbTrait — merges + associated types --------

impl SnapshotDbTrait for SnapshotKvDbMdbx {
    type SnapshotKvdbIterTraitTag = KvdbMdbxIteratorTag;
    type SnapshotKvdbIterType = PrefixedKvdbMdbx;
    type SnapshotMptDb = SnapshotKvDbMdbx;

    fn get_null_snapshot() -> Self { Self::get_null_snapshot() }

    /// Opening a real snapshot goes through the manager under
    /// MDBX — the manager owns the shared env and the semaphore.
    /// This method exists to satisfy the trait signature; every
    /// production call site goes through
    /// `SnapshotDbManagerMdbx::open_snapshot_readonly` (arriving
    /// in 5c.d) which then calls `Self::attach`.
    ///
    /// **Contract**: never called by production code post-5c.f;
    /// panics loudly so a stale caller shows up in CI, not at
    /// runtime.
    fn open(
        _snapshot_path: &std::path::Path, _readonly: bool,
        _already_open_snapshots: &crate::storage_db::AlreadyOpenSnapshots<Self>,
        _open_semaphore: &Arc<Semaphore>,
    ) -> Result<Self> {
        bail!(
            "SnapshotKvDbMdbx::open must not be called directly — \
             go through SnapshotDbManagerMdbx::open_snapshot_readonly \
             (arriving in Phase 5c.d)."
        )
    }

    /// See [`Self::open`] — same contract. Kept for
    /// trait-signature parity with the paritydb reference.
    fn create(
        _snapshot_path: &std::path::Path,
        _already_open_snapshots: &crate::storage_db::AlreadyOpenSnapshots<Self>,
        _open_semaphore: &Arc<Semaphore>, _mpt_table_in_current_db: bool,
    ) -> Result<Self> {
        bail!(
            "SnapshotKvDbMdbx::create must not be called directly — \
             go through SnapshotDbManagerMdbx (Phase 5c.d)."
        )
    }

    /// Merge this snapshot's delta dump into its MPT without
    /// reading from any parent (used when `old_snapshot_epoch_id
    /// == NULL_EPOCH` and there is no parent).
    ///
    /// Mirrors [`SnapshotKvDbParitydb::direct_merge`]:
    /// 1. Apply the delta-set / delta-del dumps to the flat KV
    ///    state (unless `recover_mpt_with_kv_snapshot_exist`).
    /// 2. Fold the delta into a fresh MPT via [`MptMerger`].
    /// 3. Return the resulting merkle root.
    ///
    /// **Chunking**: still single-txn here per the 5c.b/c
    /// design. 5c.e swaps in the chunked writer.
    fn direct_merge(
        &mut self, old_snapshot_db: Option<&Arc<Self>>,
        _mpt_snapshot: &mut Option<Self::SnapshotMptDb>,
        recover_mpt_with_kv_snapshot_exist: bool,
        in_reconstruct_snapshot_state: bool,
    ) -> Result<MerkleHash> {
        if !recover_mpt_with_kv_snapshot_exist {
            self.apply_delta_to_kv_state()?;
        }

        // If we're recovering the MPT while the KV snapshot
        // already exists (e.g. after a partial run), carry the
        // parent's MPT nodes forward. This mirrors paritydb's
        // "if old_db exists, iterate its MPT into ours" branch.
        if let Some(old_db) = old_snapshot_db {
            let mut old_iter_wrap = old_db.snapshot_mpt_iterator()?.take();
            let mut it = <PrefixedKvdbMdbx as KeyValueDbIterableTrait<
                MptKeyValue,
                [u8],
                KvdbMdbxIteratorTag,
            >>::iter_range(&mut old_iter_wrap, EMPTY_KEY, None)?
            .take();
            let new_mpt = self.mpt_view();
            while let Some((k, v)) = it.next()? {
                use crate::storage_db::KeyValueDbTrait;
                KeyValueDbTrait::put(&new_mpt, &k, &v)?;
            }
        }

        // Fold the delta into the MPT via MptMerger.
        let mut set_iter_view = self.delta_set_view();
        let mut del_iter_view = self.delta_del_view();
        let mut set_iter_wrap = <PrefixedKvdbMdbx as KeyValueDbIterableTrait<
            MptKeyValue,
            [u8],
            KvdbMdbxIteratorTag,
        >>::iter_range(
            &mut set_iter_view, EMPTY_KEY, None
        )?
        .take();
        let mut del_iter_wrap =
            <PrefixedKvdbMdbx as KeyValueDbIterableTrait<
                (Vec<u8>, ()),
                [u8],
                KvdbMdbxIteratorTag,
            >>::iter_range(&mut del_iter_view, EMPTY_KEY, None)?
            .take();
        let mut mpt_out =
            <Self as OpenSnapshotMptTrait>::open_snapshot_mpt_owned(self)?;
        let mut mpt_merger = MptMerger::new(
            None,
            &mut mpt_out as &mut dyn SnapshotMptTraitRw,
        );
        let snapshot_root = mpt_merger.merge_insertion_deletion_separated(
            &mut del_iter_wrap,
            &mut set_iter_wrap,
            in_reconstruct_snapshot_state,
        )?;
        Ok(snapshot_root)
    }

    /// Copy the parent's flat KV into this snapshot, then merge
    /// this snapshot's delta on top and rebuild the MPT.
    ///
    /// Mirrors [`SnapshotKvDbParitydb::copy_and_merge`]. Same
    /// chunking caveat as [`Self::direct_merge`].
    fn copy_and_merge(
        &mut self, old_snapshot_db: &Arc<Self>,
        _mpt_snapshot_db: &mut Option<Self::SnapshotMptDb>,
        in_reconstruct_snapshot_state: bool,
    ) -> Result<MerkleHash> {
        // Step 1: copy parent's flat KV state into ours.
        let mut old_kv_wrap = old_snapshot_db.snapshot_kv_iterator()?.take();
        let mut old_kv_it = <PrefixedKvdbMdbx as KeyValueDbIterableTrait<
            MptKeyValue,
            [u8],
            KvdbMdbxIteratorTag,
        >>::iter_range(
            &mut old_kv_wrap, EMPTY_KEY, None
        )?
        .take();
        while let Some((k, v)) = old_kv_it.next()? {
            <Self as KeyValueDbTraitSingleWriter>::put(self, &k, &v)?;
        }

        // Step 2: apply this snapshot's delta dump on top.
        self.apply_delta_to_kv_state()?;

        // Step 3: fold delta into the MPT, using the parent MPT
        // as the base so unchanged subtrees reuse its nodes.
        let mut set_iter_view = self.delta_set_view();
        let mut del_iter_view = self.delta_del_view();
        let mut set_iter_wrap = <PrefixedKvdbMdbx as KeyValueDbIterableTrait<
            MptKeyValue,
            [u8],
            KvdbMdbxIteratorTag,
        >>::iter_range(
            &mut set_iter_view, EMPTY_KEY, None
        )?
        .take();
        let mut del_iter_wrap =
            <PrefixedKvdbMdbx as KeyValueDbIterableTrait<
                (Vec<u8>, ()),
                [u8],
                KvdbMdbxIteratorTag,
            >>::iter_range(&mut del_iter_view, EMPTY_KEY, None)?
            .take();
        let mut base_mpt = old_snapshot_db.open_snapshot_mpt_as_owned()?;
        let mut save_as_mpt =
            <Self as OpenSnapshotMptTrait>::open_snapshot_mpt_owned(self)?;
        let mut mpt_merger = MptMerger::new(
            Some(&mut base_mpt as &mut dyn SnapshotMptTraitReadAndIterate),
            &mut save_as_mpt as &mut dyn SnapshotMptTraitRw,
        );
        let snapshot_root = mpt_merger.merge_insertion_deletion_separated(
            &mut del_iter_wrap,
            &mut set_iter_wrap,
            in_reconstruct_snapshot_state,
        )?;
        Ok(snapshot_root)
    }

    fn start_transaction(&mut self) -> Result<()> {
        <Self as SnapshotDbWriteableTrait>::start_transaction(self)
    }

    fn commit_transaction(&mut self) -> Result<()> {
        <Self as SnapshotDbWriteableTrait>::commit_transaction(self)
    }

    fn is_mpt_table_in_current_db(&self) -> bool {
        self.is_mpt_table_in_current_db()
    }

    fn snapshot_kv_iterator(
        &self,
    ) -> Result<
        Wrap<
            Self::SnapshotKvdbIterType,
            dyn KeyValueDbIterableTrait<
                MptKeyValue,
                [u8],
                Self::SnapshotKvdbIterTraitTag,
            >,
        >,
    > {
        Self::snapshot_kv_iterator(self)
    }
}

impl SnapshotKvDbMdbx {
    /// Walk this snapshot's `SUB_PREFIX_DELTA_SET` +
    /// `SUB_PREFIX_DELTA_DEL` dumps and apply them to the flat
    /// `SUB_PREFIX_KV` state. Called by `direct_merge` /
    /// `copy_and_merge` before folding the delta into the MPT.
    ///
    /// **Chunking**: still walks the entire delta into two `Vec`s
    /// before flushing. Fine for testnet snapshot sizes; 5c.e
    /// replaces the collect-then-flush pattern with a streaming
    /// chunked write.
    fn apply_delta_to_kv_state(&mut self) -> Result<()> {
        let mut set_view = self.delta_set_view();
        let mut del_view = self.delta_del_view();

        let mut sets = Vec::new();
        let mut dels = Vec::new();

        // Collect the delta dumps first — we can't hold the
        // iterators while calling `self.put` / `self.delete`
        // because those borrow `&mut self`.
        let mut set_wrap = <PrefixedKvdbMdbx as KeyValueDbIterableTrait<
            MptKeyValue,
            [u8],
            KvdbMdbxIteratorTag,
        >>::iter_range(&mut set_view, EMPTY_KEY, None)?
        .take();
        while let Some((k, v)) = set_wrap.next()? {
            sets.push((k, v));
        }
        drop(set_wrap);
        let mut del_wrap =
            <PrefixedKvdbMdbx as KeyValueDbIterableTrait<
                (Vec<u8>, ()),
                [u8],
                KvdbMdbxIteratorTag,
            >>::iter_range(&mut del_view, EMPTY_KEY, None)?
            .take();
        while let Some((k, _)) = del_wrap.next()? {
            dels.push(k);
        }
        drop(del_wrap);

        for k in &dels {
            <Self as KeyValueDbTraitSingleWriter>::delete(self, k)?;
        }
        for (k, v) in sets {
            <Self as KeyValueDbTraitSingleWriter>::put(self, &k, &v)?;
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

    // ------------------ 5c.c iterator + MPT tests ------------------

    /// Ranged iteration through `PrefixedKvdbMdbx`'s
    /// `KeyValueDbIterableTrait<MptKeyValue, ...>` yields exactly
    /// the keys inside the snapshot's slice, with the 33-byte
    /// prefix stripped.
    #[test]
    fn iter_range_yields_stripped_keys_in_order() {
        let (_dir, sn) = make_snapshot(0xa1);
        use crate::storage_db::KeyValueDbTrait;
        let kv = sn.kv_view();
        // Ordered insert — MDBX iteration is B+tree ordered.
        for i in 0u8..8 {
            KeyValueDbTrait::put(&kv, &[i, i], &[i]).unwrap();
        }
        // Also write a key in a neighbouring sub-table to prove
        // iteration doesn't leak sideways.
        KeyValueDbTrait::put(&sn.mpt_view(), b"leak_check", b"mpt")
            .unwrap();

        let mut view = sn.kv_view();
        let mut it = <PrefixedKvdbMdbx as KeyValueDbIterableTrait<
            MptKeyValue,
            [u8],
            KvdbMdbxIteratorTag,
        >>::iter_range(&mut view, EMPTY_KEY, None)
        .unwrap()
        .take();
        let mut seen = Vec::new();
        while let Some((k, v)) = it.next().unwrap() {
            seen.push((k, v));
        }
        assert_eq!(seen.len(), 8);
        for (i, (k, v)) in seen.iter().enumerate() {
            assert_eq!(k, &vec![i as u8, i as u8]);
            assert_eq!(v.as_ref(), &[i as u8]);
        }
    }

    /// The unit-typed variant (`Item = (Vec<u8>, ())`) yields the
    /// same key set but drops the value payload. Used by
    /// `merge_insertion_deletion_separated` for the delete iter.
    #[test]
    fn iter_range_unit_variant_drops_values() {
        let (_dir, sn) = make_snapshot(0xa2);
        use crate::storage_db::KeyValueDbTrait;
        for i in 0u8..4 {
            // Presence-only: empty value.
            KeyValueDbTrait::put(&sn.delta_del_view(), &[i], &[])
                .unwrap();
        }
        let mut view = sn.delta_del_view();
        let mut it = <PrefixedKvdbMdbx as KeyValueDbIterableTrait<
            (Vec<u8>, ()),
            [u8],
            KvdbMdbxIteratorTag,
        >>::iter_range(&mut view, EMPTY_KEY, None)
        .unwrap()
        .take();
        let mut count = 0;
        while let Some((k, ())) = it.next().unwrap() {
            assert_eq!(k.len(), 1);
            count += 1;
        }
        assert_eq!(count, 4);
    }

    /// Iterator bounds work: `[lower_incl, upper_excl)`. Insert a
    /// full range then slice it and confirm both edges.
    #[test]
    fn iter_range_bounds_are_inclusive_lower_exclusive_upper() {
        let (_dir, sn) = make_snapshot(0xa3);
        use crate::storage_db::KeyValueDbTrait;
        let kv = sn.kv_view();
        for i in 0u8..10 {
            KeyValueDbTrait::put(&kv, &[i], &[i]).unwrap();
        }
        let mut view = sn.kv_view();
        let lower = [3u8];
        let upper = [7u8];
        let mut it = <PrefixedKvdbMdbx as KeyValueDbIterableTrait<
            MptKeyValue,
            [u8],
            KvdbMdbxIteratorTag,
        >>::iter_range(
            &mut view, &lower[..], Some(&upper[..])
        )
        .unwrap()
        .take();
        let mut keys = Vec::new();
        while let Some((k, _)) = it.next().unwrap() {
            keys.push(k[0]);
        }
        assert_eq!(keys, vec![3, 4, 5, 6]);
    }

    /// `SnapshotMptLoadNode::load_node_rlp` on a fresh MPT view
    /// returns `None` for any key — the MPT slice starts empty.
    #[test]
    fn snapshot_mpt_load_node_empty() {
        let (_dir, sn) = make_snapshot(0xb0);
        let mut mpt = sn.mpt_view();
        assert!(mpt.load_node_rlp(b"anything").unwrap().is_none());
    }

    /// `SnapshotDbTrait::direct_merge` on an empty snapshot with
    /// no delta and no parent returns the null-MPT root. Smoke
    /// test — the merge machinery compiles and runs end-to-end.
    #[test]
    fn direct_merge_empty_snapshot_returns_null_root() {
        use primitives::MERKLE_NULL_NODE;
        let (_dir, mut sn) = make_snapshot(0xb1);
        let root = sn
            .direct_merge(None, &mut None, false, false)
            .expect("direct_merge on empty snapshot");
        assert_eq!(root, MERKLE_NULL_NODE);
    }

    /// `is_mpt_table_in_current_db` returns true for real
    /// snapshots — the isolated-MPT-dir mode is dead per §3 /
    /// R1.3.
    #[test]
    fn mpt_table_flag_is_always_true() {
        let (_dir, sn) = make_snapshot(0xb2);
        assert!(
            <SnapshotKvDbMdbx as SnapshotDbTrait>::
                is_mpt_table_in_current_db(&sn)
        );
    }
}
