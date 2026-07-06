// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Dual-write shadow adapter for the MDBX migration (Phase 1 step 7).
//!
//! [`MdbxShadowMirror`] wraps a *primary* KV table (typically the
//! current ParityDB column that a code path already writes) plus a
//! *shadow* [`KvdbMdbx`] column and mirrors every mutation to both.
//! Reads answer from the primary alone until parity is verified;
//! [`MdbxShadowMirror::verify_parity`] walks both sides and produces a
//! [`DualWriteReport`] enumerating any divergence.
//!
//! # Migration flow
//!
//! This is the classic shadow-then-cutover pattern. For each table
//! moving from ParityDB → MDBX in Phase 2:
//!
//! 1. **Shadow.** Wrap the existing write site in a
//!    `MdbxShadowMirror::put/delete`. Every mutation now lands in both
//!    backends. Reads keep answering from the primary — behavior
//!    unchanged for consumers.
//! 2. **Verify.** At every era boundary (or manually via a
//!    dev-tools RPC), call `verify_parity()`. A report with
//!    `is_matched() == true` for N consecutive eras means the shadow
//!    reproduces the primary faithfully.
//! 3. **Cutover.** Flip reads to the shadow. Keep the mirror in place
//!    (still writes to both) for a rollback window.
//! 4. **Drop the primary.** Remove the ParityDB column, rename the
//!    mirror to a plain [`KvdbMdbx`] read/write, retire the mirror
//!    module.
//!
//! # Phase 1 vs Phase 2 shape
//!
//! In Phase 1 (this commit) `primary` is typed as [`KvdbMdbx`] so the
//! whole file can be exercised by tests using two MDBX instances with
//! no ParityDB runtime dependency. When Phase 2 lands, the primary
//! slot is generalized behind a small [`DualWritePrimary`] trait and
//! [`kvdb_paritydb::KvdbParitydb`](super::kvdb_paritydb::KvdbParitydb)
//! implements it. The dual-write logic in this file does not change;
//! only the type of `primary` moves from `KvdbMdbx` to
//! `dyn DualWritePrimary`.
//!
//! # Column identity
//!
//! [`MdbxShadowMirror::new`] takes a [`Column`] value (from
//! [`super::mdbx_columns`]) so the emitted [`DualWriteReport`] names
//! the logical table under audit — a report against
//! `Column::HashByNumber` reads as "HashByNumber: 0 diverged" rather
//! than an anonymous "column 7".

use super::{
    kvdb_mdbx::{BatchOp, KvdbMdbx},
    mdbx_columns::Column,
};
use crate::impls::errors::*;

/// The minimal KV surface [`MdbxShadowMirror`] needs from its primary
/// backend during the shadow phase of a migration.
///
/// Implemented for [`KvdbMdbx`] (so tests can exercise the whole
/// dual-write module using two MDBX instances without pulling
/// ParityDB into the test path) and, in Phase 2, for
/// [`KvdbParitydb`](super::kvdb_paritydb::KvdbParitydb) (so a real
/// ParityDB column can act as the primary during a table swap).
///
/// The trait deliberately doesn't inherit from the existing
/// `KeyValueDbTrait` / `KeyValueDbTraitRead` / `KeyValueDbIterableTrait`
/// family — those carry generic wrapping / lifetime machinery that
/// makes them awkward to hold as a trait object or to blanket-impl on
/// a new backend. Four small methods, all bytes-in / owned-bytes-out,
/// no lifetime dance.
///
/// **Ownership**: the primary is passed by value into
/// [`MdbxShadowMirror::new`] and lives as long as the mirror. Backends
/// that are cheap to `Clone` (both `KvdbMdbx` and `KvdbParitydb` share
/// an `Arc<Env>` under the hood) satisfy this without churn.
///
/// **Thread safety**: implementations must be `Send + Sync` because
/// the mirror is used across the executor + async-persistence +
/// parity-auditor threads. Both existing backends already meet this
/// bound.
pub trait DualWritePrimary: Send + Sync {
    /// Upsert a single key. Overwrites any prior value.
    fn dw_put(&self, k: &[u8], v: &[u8]) -> Result<()>;

    /// Remove a key. Silent no-op if the key is absent — matches
    /// [`KeyValueDbTrait::delete`](crate::storage_db::key_value_db::
    /// KeyValueDbTrait::delete).
    fn dw_delete(&self, k: &[u8]) -> Result<()>;

    /// Look up a key. Returns `Ok(None)` if the key isn't present.
    fn dw_get(&self, k: &[u8]) -> Result<Option<Box<[u8]>>>;

    /// Snapshot-consistent range scan over `[lower_incl, upper_excl)`.
    /// Results in ascending key order. `upper_excl = None` iterates
    /// to the end of the column. Materialized into a `Vec` — same
    /// contract as [`KvdbMdbx::iter_range_owned`].
    ///
    /// Consumed by [`MdbxShadowMirror::verify_parity`], which walks
    /// both backends in full and classifies divergences. This is not
    /// intended for hot-path reads.
    fn dw_iter_range(
        &self, lower_incl: &[u8], upper_excl: Option<&[u8]>,
    ) -> Result<Vec<(Box<[u8]>, Box<[u8]>)>>;
}

impl DualWritePrimary for KvdbMdbx {
    fn dw_put(&self, k: &[u8], v: &[u8]) -> Result<()> {
        use crate::storage_db::key_value_db::KeyValueDbTrait;
        self.put(k, v).map(|_| ())
    }

    fn dw_delete(&self, k: &[u8]) -> Result<()> {
        use crate::storage_db::key_value_db::KeyValueDbTrait;
        self.delete(k).map(|_| ())
    }

    fn dw_get(&self, k: &[u8]) -> Result<Option<Box<[u8]>>> {
        use crate::storage_db::key_value_db::KeyValueDbTraitRead;
        self.get(k)
    }

    fn dw_iter_range(
        &self, lower_incl: &[u8], upper_excl: Option<&[u8]>,
    ) -> Result<Vec<(Box<[u8]>, Box<[u8]>)>> {
        self.iter_range_owned(lower_incl, upper_excl)
    }
}

impl DualWritePrimary for super::kvdb_paritydb::KvdbParitydb {
    fn dw_put(&self, k: &[u8], v: &[u8]) -> Result<()> {
        use crate::storage_db::key_value_db::KeyValueDbTrait;
        self.put(k, v).map(|_| ())
    }

    fn dw_delete(&self, k: &[u8]) -> Result<()> {
        use crate::storage_db::key_value_db::KeyValueDbTrait;
        self.delete(k).map(|_| ())
    }

    fn dw_get(&self, k: &[u8]) -> Result<Option<Box<[u8]>>> {
        use crate::storage_db::key_value_db::KeyValueDbTraitRead;
        self.get(k)
    }

    fn dw_iter_range(
        &self, lower_incl: &[u8], upper_excl: Option<&[u8]>,
    ) -> Result<Vec<(Box<[u8]>, Box<[u8]>)>> {
        // paritydb's boxed `iter(col)` walks the entire column. We
        // filter by `[lower_incl, upper_excl)` in-line, then sort by
        // key to match [`KvdbMdbx::iter_range_owned`]'s ascending
        // contract. paritydb's on-disk order isn't guaranteed to
        // match MDBX's B+tree order across all builds, so the
        // explicit sort at the end keeps the two backends
        // parity-comparable by simply zipping their outputs.
        //
        // Materialised into `Vec` for the same reason as the MDBX
        // variant: verify_parity is not a hot-path caller, and a
        // streaming iterator would require self-referential lifetime
        // management here (the paritydb iterator borrows from
        // `Arc<dyn KeyValueStore>` and can't easily be handed back
        // through a plain `Vec` API). Bounded by the column's
        // populated key count — deliberate for the audit path.
        let mut out: Vec<(Box<[u8]>, Box<[u8]>)> = Vec::new();
        for (k, v) in self.kvdb.iter(self.col) {
            if k.as_ref() < lower_incl {
                continue;
            }
            if let Some(upper) = upper_excl {
                if k.as_ref() >= upper {
                    continue;
                }
            }
            out.push((k, v));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }
}

/// One-shot divergence report between a primary KV table and its
/// [`KvdbMdbx`] shadow, emitted by
/// [`MdbxShadowMirror::verify_parity`].
///
/// The `missing_in_shadow` / `extra_in_shadow` / `value_mismatches`
/// vectors are populated with the *keys* that diverge — enough to
/// pinpoint where the shadow wrote wrong data without exploding the
/// report size for a large table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DualWriteReport {
    /// Human-readable column name (e.g. `"HashByNumber"`).
    pub table: &'static str,
    /// Number of entries observed in the primary.
    pub primary_count: usize,
    /// Number of entries observed in the shadow.
    pub shadow_count: usize,
    /// Keys present in the primary but absent from the shadow.
    pub missing_in_shadow: Vec<Box<[u8]>>,
    /// Keys present in the shadow but absent from the primary.
    pub extra_in_shadow: Vec<Box<[u8]>>,
    /// Keys present on both sides with a different value.
    pub value_mismatches: Vec<Box<[u8]>>,
}

impl DualWriteReport {
    /// The shadow reproduces the primary exactly on every key/value
    /// pair. Cutover to shadow-as-primary is safe.
    pub fn is_matched(&self) -> bool {
        self.missing_in_shadow.is_empty()
            && self.extra_in_shadow.is_empty()
            && self.value_mismatches.is_empty()
            && self.primary_count == self.shadow_count
    }

    /// Total number of divergent keys.
    pub fn diverged_count(&self) -> usize {
        self.missing_in_shadow.len()
            + self.extra_in_shadow.len()
            + self.value_mismatches.len()
    }
}

/// Dual-write helper: puts and deletes mirror to both primary and
/// shadow; reads answer from the primary; [`Self::verify_parity`]
/// audits both sides against each other.
///
/// Generic over `P: DualWritePrimary` so the same struct can wrap
/// either a `KvdbMdbx` (unit tests) or a `KvdbParitydb` (Phase 2 real
/// swap) as its primary side. The shadow is always [`KvdbMdbx`].
///
/// Where a mirror's [`MdbxShadowMirror::get`] answers from during
/// the migration lifecycle. Writes always land on both backends
/// regardless of this setting; only read routing changes.
///
/// - [`ReadSource::Primary`] — original behaviour. Reads answer
///   from the ParityDB primary; the shadow is write-only. Safe
///   default while parity is still being validated.
/// - [`ReadSource::ShadowWithPrimaryFallback`] — read from the
///   shadow first; on miss, fall back to the primary. This is the
///   staging mode for Phase 3 read cutover: reads that hit the
///   shadow ARE using the new hot tier, but any pre-flag key that
///   never made it to the shadow still resolves via the primary.
///   Adds one extra MDBX read on every primary hit.
/// - [`ReadSource::Shadow`] — reads answer only from the shadow.
///   Cutover complete; the primary is about to be dropped. Any
///   key not in the shadow will surface as `Ok(None)` — the
///   caller sees the same absence it would after a wipe.
///
/// Kept as an atomic-swappable field so an operator RPC can flip
/// modes at runtime without a full node restart. See
/// [`MdbxShadowMirror::set_read_source`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadSource {
    Primary,
    ShadowWithPrimaryFallback,
    Shadow,
}

/// See module-level doc for the migration flow this enables.
pub struct MdbxShadowMirror<P: DualWritePrimary> {
    primary: P,
    shadow: KvdbMdbx,
    column: Column,
    /// Which backend [`Self::get`] answers from. Atomic so that
    /// concurrent readers observe a coherent single value even
    /// while an operator is flipping cutover on this table.
    read_source: std::sync::atomic::AtomicU8,
}

// Encoding for the atomic: keep u8 stable so future variants
// slot in without shifting.
const READ_SOURCE_PRIMARY: u8 = 0;
const READ_SOURCE_SHADOW_WITH_FALLBACK: u8 = 1;
const READ_SOURCE_SHADOW: u8 = 2;

fn encode_read_source(r: ReadSource) -> u8 {
    match r {
        ReadSource::Primary => READ_SOURCE_PRIMARY,
        ReadSource::ShadowWithPrimaryFallback => {
            READ_SOURCE_SHADOW_WITH_FALLBACK
        }
        ReadSource::Shadow => READ_SOURCE_SHADOW,
    }
}
fn decode_read_source(v: u8) -> ReadSource {
    match v {
        READ_SOURCE_PRIMARY => ReadSource::Primary,
        READ_SOURCE_SHADOW_WITH_FALLBACK => {
            ReadSource::ShadowWithPrimaryFallback
        }
        READ_SOURCE_SHADOW => ReadSource::Shadow,
        // Unknown value shouldn't happen (we only write via
        // encode); default to Primary as the safe fallback.
        _ => ReadSource::Primary,
    }
}

impl<P: DualWritePrimary> MdbxShadowMirror<P> {
    /// Construct a mirror from a primary handle (any
    /// [`DualWritePrimary`] impl), an MDBX shadow column, and the
    /// [`Column`] identity used to name audit reports. Starts in
    /// [`ReadSource::Primary`] mode — safe default until parity
    /// is validated.
    pub fn new(primary: P, shadow: KvdbMdbx, column: Column) -> Self {
        Self {
            primary,
            shadow,
            column,
            read_source: std::sync::atomic::AtomicU8::new(
                READ_SOURCE_PRIMARY,
            ),
        }
    }

    /// Which side [`Self::get`] will currently answer from.
    pub fn read_source(&self) -> ReadSource {
        decode_read_source(
            self.read_source
                .load(std::sync::atomic::Ordering::Acquire),
        )
    }

    /// Atomically flip the read source at runtime. Concurrent
    /// [`Self::get`] callers observe either the old or the new
    /// source but never a torn value.
    pub fn set_read_source(&self, r: ReadSource) {
        self.read_source.store(
            encode_read_source(r),
            std::sync::atomic::Ordering::Release,
        );
    }

    /// Mirror a single-key write to both backends.
    ///
    /// **Atomicity**: this is a shadow adapter, not a transactional
    /// mirror. If the shadow write fails after the primary succeeds,
    /// the two backends diverge; the next `verify_parity` catches it
    /// and the operator can either replay from the primary or roll
    /// the shadow forward via a targeted put. This is deliberate —
    /// promoting the shadow to primary is a manual cutover, so a
    /// transient divergence isn't a correctness bug at the primary,
    /// only a signal that the shadow needs repair before cutover.
    pub fn put(&self, k: &[u8], v: &[u8]) -> Result<()> {
        use crate::storage_db::key_value_db::KeyValueDbTrait;
        self.primary.dw_put(k, v)?;
        self.shadow.put(k, v)?;
        Ok(())
    }

    /// Mirror a delete to both backends.
    pub fn delete(&self, k: &[u8]) -> Result<()> {
        use crate::storage_db::key_value_db::KeyValueDbTrait;
        self.primary.dw_delete(k)?;
        self.shadow.delete(k)?;
        Ok(())
    }

    /// Mirror a batch of writes. The primary uses per-op puts (matches
    /// today's ParityDB call pattern); the shadow uses [`KvdbMdbx::
    /// write_batch`] so all shadow ops land in one MDBX rw_txn. This
    /// keeps shadow throughput near what the executor will see
    /// post-cutover.
    pub fn write_batch(&self, ops: &[BatchOp]) -> Result<()> {
        for op in ops {
            match op {
                BatchOp::Put(k, v) => {
                    self.primary.dw_put(k, v)?;
                }
                BatchOp::Delete(k) => {
                    self.primary.dw_delete(k)?;
                }
            }
        }
        self.shadow.write_batch(ops)?;
        Ok(())
    }

    /// Read the mirrored key. Where the answer comes from depends
    /// on [`Self::read_source`]:
    ///
    /// - `Primary` — the pre-cutover default; reads only from
    ///   ParityDB.
    /// - `ShadowWithPrimaryFallback` — try MDBX first; on a miss
    ///   fall back to ParityDB. Any key not yet materialized in
    ///   the shadow still resolves. Cost: one extra MDBX read on
    ///   every primary hit.
    /// - `Shadow` — read only from MDBX. Keys not in the shadow
    ///   return `Ok(None)`, matching the post-wipe view.
    ///
    /// Called on the RPC hot path (`load_from_db` in the
    /// `BlockDataManager`); the atomic load is a single
    /// unsynchronized memory read.
    pub fn get(&self, k: &[u8]) -> Result<Option<Box<[u8]>>> {
        use crate::storage_db::key_value_db::KeyValueDbTraitRead;
        match self.read_source() {
            ReadSource::Primary => self.primary.dw_get(k),
            ReadSource::ShadowWithPrimaryFallback => {
                match self.shadow.get(k)? {
                    Some(v) => Ok(Some(v)),
                    None => self.primary.dw_get(k),
                }
            }
            ReadSource::Shadow => self.shadow.get(k),
        }
    }

    /// Walk both backends in full and produce a
    /// [`DualWriteReport`] enumerating divergences.
    ///
    /// **Memory**: this materializes both column contents into `Vec`s
    /// so it's not something to call on the hot path. Intended for
    /// era-boundary audits and manual dev-tools RPC checks. The
    /// underlying `dw_iter_range` /
    /// [`KvdbMdbx::iter_range_owned`] are snapshot-consistent per
    /// backend but the two snapshots aren't atomic against each
    /// other — take the report during a quiescent window (e.g.
    /// immediately after execution commit, before the next epoch's
    /// mutations).
    pub fn verify_parity(&self) -> Result<DualWriteReport> {
        let primary = self.primary.dw_iter_range(b"", None)?;
        let shadow = self.shadow.iter_range_owned(b"", None)?;

        // Both are already ascending by key (guaranteed by
        // iter_range_owned). Walk them in lockstep and classify
        // every divergence.
        let mut missing_in_shadow: Vec<Box<[u8]>> = Vec::new();
        let mut extra_in_shadow: Vec<Box<[u8]>> = Vec::new();
        let mut value_mismatches: Vec<Box<[u8]>> = Vec::new();

        let mut i = 0usize;
        let mut j = 0usize;
        while i < primary.len() && j < shadow.len() {
            let (pk, pv) = &primary[i];
            let (sk, sv) = &shadow[j];
            match pk.cmp(sk) {
                std::cmp::Ordering::Equal => {
                    if pv != sv {
                        value_mismatches.push(pk.clone());
                    }
                    i += 1;
                    j += 1;
                }
                std::cmp::Ordering::Less => {
                    missing_in_shadow.push(pk.clone());
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    extra_in_shadow.push(sk.clone());
                    j += 1;
                }
            }
        }
        while i < primary.len() {
            missing_in_shadow.push(primary[i].0.clone());
            i += 1;
        }
        while j < shadow.len() {
            extra_in_shadow.push(shadow[j].0.clone());
            j += 1;
        }

        Ok(DualWriteReport {
            table: self.column.name(),
            primary_count: primary.len(),
            shadow_count: shadow.len(),
            missing_in_shadow,
            extra_in_shadow,
            value_mismatches,
        })
    }

    /// Test-only escape hatch: return the underlying shadow handle so
    /// tests can inject a divergence directly and prove
    /// [`Self::verify_parity`] catches it. Not exposed in prod code
    /// paths (they should never bypass the mirror).
    #[cfg(test)]
    pub(crate) fn shadow_for_test(&self) -> &KvdbMdbx {
        &self.shadow
    }

    /// Test-only escape hatch for injecting into the primary side.
    /// Returns a reference of the generic primary type — callers can
    /// use its concrete trait impls directly.
    #[cfg(test)]
    pub(crate) fn primary_for_test(&self) -> &P {
        &self.primary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impls::storage_db::kvdb_mdbx::MdbxEnv;
    use crate::storage_db::key_value_db::KeyValueDbTrait;
    use std::sync::Arc;

    fn make_mirror(
        column: Column,
    ) -> (
        tempdir::TempDir,
        tempdir::TempDir,
        MdbxShadowMirror<KvdbMdbx>,
    ) {
        let dir_a = tempdir::TempDir::new("mdbx_dw_primary").unwrap();
        let dir_b = tempdir::TempDir::new("mdbx_dw_shadow").unwrap();
        let primary = KvdbMdbx::with_column(
            MdbxEnv::open(dir_a.path()).unwrap(),
            0,
        );
        let shadow = KvdbMdbx::with_column(
            MdbxEnv::open(dir_b.path()).unwrap(),
            0,
        );
        let mirror = MdbxShadowMirror::new(primary, shadow, column);
        (dir_a, dir_b, mirror)
    }

    /// Fresh mirror: `verify_parity` succeeds against two empty
    /// backends. Report reflects the empty state. `is_matched` is
    /// true — cutover would be safe (trivially, no data to lose).
    #[test]
    fn empty_mirror_matches() {
        let (_a, _b, mirror) = make_mirror(Column::HashByNumber);
        let report = mirror.verify_parity().unwrap();
        assert!(report.is_matched());
        assert_eq!(report.primary_count, 0);
        assert_eq!(report.shadow_count, 0);
        assert_eq!(report.diverged_count(), 0);
        assert_eq!(report.table, "HashByNumber");
    }

    /// Steady-state operation: a mix of puts, overwrites, and
    /// deletes routed through the mirror keeps both backends in sync.
    /// `verify_parity` returns matched throughout.
    #[test]
    fn steady_state_mirror_matches() {
        let (_a, _b, mirror) = make_mirror(Column::HashByNumber);

        // 40 puts, some overwrites of the same key, some deletes.
        for i in 0u64..40 {
            mirror
                .put(&i.to_be_bytes(), format!("v{}", i).as_bytes())
                .unwrap();
        }
        for i in 0u64..10 {
            mirror
                .put(
                    &i.to_be_bytes(),
                    format!("v{}-overwritten", i).as_bytes(),
                )
                .unwrap();
        }
        for i in 30u64..35 {
            mirror.delete(&i.to_be_bytes()).unwrap();
        }

        let report = mirror.verify_parity().unwrap();
        assert!(report.is_matched(), "report: {:?}", report);
        assert_eq!(report.primary_count, 35); // 40 - 5 deleted
        assert_eq!(report.shadow_count, 35);
    }

    /// The batched mirror path (used by executor commits) produces
    /// the same final state as individual puts. Verifies
    /// `write_batch` on the mirror ≡ many `put` calls at the parity
    /// level.
    #[test]
    fn batched_writes_mirror_matches() {
        let (_a, _b, mirror) = make_mirror(Column::PlainAccount);

        // Own the buffers, then borrow into BatchOp so the ops live
        // for the mirror call. Same pattern used by
        // `kvdb_mdbx::tests::stats_zero_before_writes_grow_after`.
        let owned: Vec<(Vec<u8>, Vec<u8>)> = (0u64..25)
            .map(|i| (i.to_be_bytes().to_vec(), format!("val{}", i).into_bytes()))
            .collect();
        let ops: Vec<BatchOp> = owned
            .iter()
            .map(|(k, v)| BatchOp::Put(k.as_slice(), v.as_slice()))
            .collect();
        mirror.write_batch(&ops).unwrap();

        let report = mirror.verify_parity().unwrap();
        assert!(report.is_matched(), "report: {:?}", report);
        assert_eq!(report.primary_count, 25);
    }

    /// `get()` routes through the primary. Verifies the shadow is
    /// write-only during the shadow phase — reads never leak.
    #[test]
    fn get_answers_from_primary_only() {
        let (_a, _b, mirror) = make_mirror(Column::HashByNumber);
        mirror.put(b"k", b"from-primary").unwrap();

        // Tamper with the shadow directly so it diverges from the
        // primary. `get()` must NOT observe this — it reads primary.
        mirror
            .shadow_for_test()
            .put(b"k", b"tampered-shadow")
            .unwrap();

        assert_eq!(
            mirror.get(b"k").unwrap().as_deref(),
            Some(&b"from-primary"[..])
        );
        // Parity check flags it.
        let report = mirror.verify_parity().unwrap();
        assert!(!report.is_matched());
        assert_eq!(report.value_mismatches.len(), 1);
    }

    /// Default `read_source` is `Primary` — the safe pre-cutover
    /// default. Set/get round-trips correctly on every variant.
    #[test]
    fn read_source_defaults_to_primary_and_can_flip() {
        let (_a, _b, mirror) = make_mirror(Column::HashByNumber);
        assert_eq!(mirror.read_source(), ReadSource::Primary);

        mirror.set_read_source(ReadSource::ShadowWithPrimaryFallback);
        assert_eq!(
            mirror.read_source(),
            ReadSource::ShadowWithPrimaryFallback
        );

        mirror.set_read_source(ReadSource::Shadow);
        assert_eq!(mirror.read_source(), ReadSource::Shadow);

        mirror.set_read_source(ReadSource::Primary);
        assert_eq!(mirror.read_source(), ReadSource::Primary);
    }

    /// `Shadow` mode reads only from MDBX. A key that only lives
    /// on the primary — the pre-flag debt case — resolves to
    /// `Ok(None)`, matching the post-wipe view a fully-cutover
    /// node will see.
    #[test]
    fn read_source_shadow_ignores_primary_only_keys() {
        let (_a, _b, mirror) = make_mirror(Column::HashByNumber);
        // Simulate a pre-flag key that landed only on the primary.
        use crate::storage_db::key_value_db::KeyValueDbTrait;
        mirror
            .primary_for_test()
            .put(b"pre-flag", b"only-primary")
            .unwrap();
        mirror.set_read_source(ReadSource::Shadow);
        assert_eq!(mirror.get(b"pre-flag").unwrap(), None);
    }

    /// `ShadowWithPrimaryFallback` reads from MDBX when the key
    /// exists there, and falls back to the primary otherwise —
    /// the safe staging mode for Phase 3.
    #[test]
    fn read_source_shadow_with_fallback_covers_both() {
        let (_a, _b, mirror) = make_mirror(Column::HashByNumber);
        // `mirror-key` gets both sides (normal mirror path).
        mirror.put(b"mirror-key", b"both").unwrap();
        // `primary-only` simulates pre-flag debt.
        use crate::storage_db::key_value_db::KeyValueDbTrait;
        mirror
            .primary_for_test()
            .put(b"primary-only", b"only-primary")
            .unwrap();

        mirror.set_read_source(ReadSource::ShadowWithPrimaryFallback);
        assert_eq!(
            mirror.get(b"mirror-key").unwrap().as_deref(),
            Some(&b"both"[..])
        );
        assert_eq!(
            mirror.get(b"primary-only").unwrap().as_deref(),
            Some(&b"only-primary"[..])
        );
        assert_eq!(mirror.get(b"absent").unwrap(), None);
    }

    /// `Shadow` mode reads a shadow-only value even if the primary
    /// disagrees. Verifies the switch really flips: after a flip
    /// the same `get()` sees the shadow's answer, not primary's.
    #[test]
    fn read_source_shadow_reads_shadow_value_after_flip() {
        let (_a, _b, mirror) = make_mirror(Column::HashByNumber);
        mirror.put(b"k", b"initial").unwrap();

        // Tamper with the shadow directly (simulates operator
        // backfill / repair). Primary keeps `initial`.
        use crate::storage_db::key_value_db::KeyValueDbTrait;
        mirror
            .shadow_for_test()
            .put(b"k", b"repaired-in-shadow")
            .unwrap();

        // Before flip: primary answer.
        assert_eq!(
            mirror.get(b"k").unwrap().as_deref(),
            Some(&b"initial"[..])
        );
        // After flip: shadow answer.
        mirror.set_read_source(ReadSource::Shadow);
        assert_eq!(
            mirror.get(b"k").unwrap().as_deref(),
            Some(&b"repaired-in-shadow"[..])
        );
    }

    /// A key present only in the primary shows up in
    /// `missing_in_shadow`. Simulates a bug where the mirror's
    /// shadow write failed silently (should never happen in the real
    /// adapter — this catches regressions of that invariant).
    #[test]
    fn missing_in_shadow_is_reported() {
        let (_a, _b, mirror) = make_mirror(Column::PlainStorage);
        // Write to the primary directly, bypassing the mirror.
        mirror.primary_for_test().put(b"lonely", b"v").unwrap();

        let report = mirror.verify_parity().unwrap();
        assert_eq!(report.primary_count, 1);
        assert_eq!(report.shadow_count, 0);
        assert_eq!(report.missing_in_shadow.len(), 1);
        assert_eq!(&*report.missing_in_shadow[0], &b"lonely"[..]);
        assert!(!report.is_matched());
    }

    /// A key present only in the shadow shows up in `extra_in_shadow`.
    /// Simulates a stale write leaked into the shadow after a
    /// primary delete — should never happen; this is the audit
    /// invariant.
    #[test]
    fn extra_in_shadow_is_reported() {
        let (_a, _b, mirror) = make_mirror(Column::PlainStorage);
        mirror.shadow_for_test().put(b"orphan", b"v").unwrap();

        let report = mirror.verify_parity().unwrap();
        assert_eq!(report.primary_count, 0);
        assert_eq!(report.shadow_count, 1);
        assert_eq!(report.extra_in_shadow.len(), 1);
        assert_eq!(&*report.extra_in_shadow[0], &b"orphan"[..]);
        assert!(!report.is_matched());
    }

    /// Verify the report classifies mixed divergence cleanly: some
    /// matches, some primary-only, some shadow-only, some value
    /// mismatches. This is the shape a real bug would produce, and
    /// the operator dashboard wants each class separately.
    #[test]
    fn mixed_divergence_classification() {
        let (_a, _b, mirror) = make_mirror(Column::EpochBlocks);

        // Matching pair — should not appear in any divergence list.
        mirror.put(b"match", b"v").unwrap();

        // Value mismatch: same key, different value.
        mirror.put(b"vmiss", b"primary").unwrap();
        mirror
            .shadow_for_test()
            .delete(b"vmiss")
            .unwrap();
        mirror.shadow_for_test().put(b"vmiss", b"shadow").unwrap();

        // Primary-only.
        mirror.primary_for_test().put(b"ponly", b"v").unwrap();

        // Shadow-only.
        mirror.shadow_for_test().put(b"sonly", b"v").unwrap();

        let report = mirror.verify_parity().unwrap();
        assert!(!report.is_matched());
        assert_eq!(report.diverged_count(), 3);
        assert_eq!(report.value_mismatches.len(), 1);
        assert_eq!(&*report.value_mismatches[0], &b"vmiss"[..]);
        assert_eq!(report.missing_in_shadow.len(), 1);
        assert_eq!(&*report.missing_in_shadow[0], &b"ponly"[..]);
        assert_eq!(report.extra_in_shadow.len(), 1);
        assert_eq!(&*report.extra_in_shadow[0], &b"sonly"[..]);
    }

    /// `Arc<MdbxShadowMirror>` compiles — the mirror is used across
    /// threads (executor + async persistence + parity auditor).
    /// Sanity check that the traits are satisfied.
    #[test]
    fn is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MdbxShadowMirror<KvdbMdbx>>();
        assert_send_sync::<Arc<MdbxShadowMirror<KvdbMdbx>>>();
    }

    /// The `DualWritePrimary` trait is object-safe enough for the
    /// mirror to be constructed from a `Box<dyn DualWritePrimary>`
    /// (Phase 2 uses this to inject a KvdbParitydb primary at
    /// runtime without knowing its concrete type at the call site).
    ///
    /// If the trait ever grows a generic method or a `Self`-returning
    /// method, this test fails at compile time — a load-bearing
    /// property for the migration story.
    #[test]
    fn primary_trait_is_object_safe() {
        fn _needs_dyn(_: Box<dyn DualWritePrimary>) {}
    }

    // ------------------------------------------------------------------
    // Integration: KvdbParitydb primary + KvdbMdbx shadow
    // ------------------------------------------------------------------
    //
    // These tests prove the migration story end-to-end: a real
    // ParityDB column acts as the primary, an MDBX column as the
    // shadow, and `verify_parity` reports zero divergence after a
    // mixed workload. Phase 2 step 2 wires this pattern into the
    // actual `BlockDataManager` write sites for `HashByNumber`.

    use crate::impls::storage_db::kvdb_paritydb::KvdbParitydb;
    use ::db::{open_database, paritydb_settings, ParityDbOpenConfig};

    /// Open a fresh ParityDB env at `path` with a single column and
    /// return a `KvdbParitydb` handle bound to column 0. Mirrors the
    /// `open_database` + `paritydb_settings` pair used everywhere
    /// else in the crate — the on-disk layout the test exercises is
    /// exactly what production ships.
    fn open_paritydb(path: &std::path::Path) -> KvdbParitydb {
        let cfg = ParityDbOpenConfig {
            columns: 1,
            compression: None,
            disable_wal: true, // faster tempdir cleanup, safe for tests
            stats: false,
        };
        let settings =
            paritydb_settings(path.to_path_buf(), &cfg).unwrap();
        let sys_db = open_database(&settings).unwrap();
        // SystemDB wraps `Arc<dyn KeyValueStore>` internally; hand
        // that out via `.key_value()` for KvdbParitydb.
        KvdbParitydb { kvdb: sys_db.key_value(), col: 0 }
    }

    /// ParityDB primary + MDBX shadow, empty. Parity report says
    /// matched. Proves the trait impl for KvdbParitydb correctly
    /// reports zero entries when the column is empty.
    #[test]
    fn paritydb_primary_empty_matches() {
        let paritydb_dir =
            tempdir::TempDir::new("dw_pdb_empty").unwrap();
        let mdbx_dir =
            tempdir::TempDir::new("dw_mdbx_empty").unwrap();
        let primary = open_paritydb(paritydb_dir.path());
        let shadow = KvdbMdbx::with_column(
            MdbxEnv::open(mdbx_dir.path()).unwrap(),
            0,
        );
        let mirror = MdbxShadowMirror::new(
            primary,
            shadow,
            Column::HashByNumber,
        );
        let report = mirror.verify_parity().unwrap();
        assert!(report.is_matched());
        assert_eq!(report.primary_count, 0);
        assert_eq!(report.shadow_count, 0);
    }

    /// ParityDB primary + MDBX shadow, mixed workload. Every mutation
    /// goes through the mirror; both backends stay in sync; the
    /// final parity report is matched. This is the end-to-end proof
    /// that Phase 2 can safely start swapping tables.
    #[test]
    fn paritydb_primary_steady_state_matches() {
        let paritydb_dir =
            tempdir::TempDir::new("dw_pdb_steady").unwrap();
        let mdbx_dir =
            tempdir::TempDir::new("dw_mdbx_steady").unwrap();
        let primary = open_paritydb(paritydb_dir.path());
        let shadow = KvdbMdbx::with_column(
            MdbxEnv::open(mdbx_dir.path()).unwrap(),
            0,
        );
        let mirror = MdbxShadowMirror::new(
            primary,
            shadow,
            Column::HashByNumber,
        );

        // Same 40-put / 10-overwrite / 5-delete shape as the pure-
        // MDBX steady_state test, so failures narrow to "paritydb
        // impl broken" not "workload broken".
        for i in 0u64..40 {
            mirror
                .put(&i.to_be_bytes(), format!("v{}", i).as_bytes())
                .unwrap();
        }
        for i in 0u64..10 {
            mirror
                .put(
                    &i.to_be_bytes(),
                    format!("v{}-overwritten", i).as_bytes(),
                )
                .unwrap();
        }
        for i in 30u64..35 {
            mirror.delete(&i.to_be_bytes()).unwrap();
        }

        let report = mirror.verify_parity().unwrap();
        assert!(report.is_matched(), "report: {:?}", report);
        assert_eq!(report.primary_count, 35);
        assert_eq!(report.shadow_count, 35);
    }

    /// ParityDB primary + MDBX shadow: bypass the mirror on the
    /// primary side (direct paritydb write) and confirm
    /// `verify_parity` classifies it as `missing_in_shadow`. Proves
    /// the classification logic works against the paritydb iterator
    /// output the same as against MDBX's iter_range_owned.
    #[test]
    fn paritydb_primary_divergence_is_reported() {
        use crate::storage_db::key_value_db::KeyValueDbTrait;

        let paritydb_dir =
            tempdir::TempDir::new("dw_pdb_diverge").unwrap();
        let mdbx_dir =
            tempdir::TempDir::new("dw_mdbx_diverge").unwrap();
        let primary = open_paritydb(paritydb_dir.path());
        let shadow = KvdbMdbx::with_column(
            MdbxEnv::open(mdbx_dir.path()).unwrap(),
            0,
        );
        let mirror = MdbxShadowMirror::new(
            primary,
            shadow,
            Column::HashByNumber,
        );

        // Two matching pairs.
        mirror.put(b"a", b"1").unwrap();
        mirror.put(b"b", b"2").unwrap();

        // Bypass — write to paritydb primary directly.
        mirror.primary_for_test().put(b"orphan", b"lonely").unwrap();

        let report = mirror.verify_parity().unwrap();
        assert!(!report.is_matched());
        assert_eq!(report.missing_in_shadow.len(), 1);
        assert_eq!(&*report.missing_in_shadow[0], &b"orphan"[..]);
        assert_eq!(report.extra_in_shadow.len(), 0);
        assert_eq!(report.value_mismatches.len(), 0);
    }
}
