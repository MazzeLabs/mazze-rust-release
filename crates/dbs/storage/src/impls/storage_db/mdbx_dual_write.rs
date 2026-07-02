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
/// See module-level doc for the migration flow this enables.
pub struct MdbxShadowMirror {
    primary: KvdbMdbx,
    shadow: KvdbMdbx,
    column: Column,
}

impl MdbxShadowMirror {
    /// Construct a mirror from an owned pair of MDBX handles and the
    /// [`Column`] identity used to name the audit reports.
    ///
    /// **Phase 2 note**: the primary slot will move to a generic
    /// [`DualWritePrimary`] trait; the KvdbParitydb variant of that
    /// trait will let this same struct wrap the current ParityDB
    /// column at each write site. The MDBX-typed signature here is a
    /// Phase-1 stand-in that lets the whole file be unit-tested
    /// without pulling ParityDB into the test path.
    pub fn new(
        primary: KvdbMdbx, shadow: KvdbMdbx, column: Column,
    ) -> Self {
        Self { primary, shadow, column }
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
        self.primary.put(k, v)?;
        self.shadow.put(k, v)?;
        Ok(())
    }

    /// Mirror a delete to both backends.
    pub fn delete(&self, k: &[u8]) -> Result<()> {
        use crate::storage_db::key_value_db::KeyValueDbTrait;
        self.primary.delete(k)?;
        self.shadow.delete(k)?;
        Ok(())
    }

    /// Mirror a batch of writes. The primary uses per-op puts (matches
    /// today's ParityDB call pattern); the shadow uses [`KvdbMdbx::
    /// write_batch`] so all shadow ops land in one MDBX rw_txn. This
    /// keeps shadow throughput near what the executor will see
    /// post-cutover.
    pub fn write_batch(&self, ops: &[BatchOp]) -> Result<()> {
        use crate::storage_db::key_value_db::KeyValueDbTrait;
        for op in ops {
            match op {
                BatchOp::Put(k, v) => {
                    self.primary.put(k, v)?;
                }
                BatchOp::Delete(k) => {
                    self.primary.delete(k)?;
                }
            }
        }
        self.shadow.write_batch(ops)?;
        Ok(())
    }

    /// Read via the primary. During the shadow phase this is the
    /// only path — the shadow is write-only until cutover.
    pub fn get(&self, k: &[u8]) -> Result<Option<Box<[u8]>>> {
        use crate::storage_db::key_value_db::KeyValueDbTraitRead;
        self.primary.get(k)
    }

    /// Walk both backends in full and produce a
    /// [`DualWriteReport`] enumerating divergences.
    ///
    /// **Memory**: this materializes both column contents into `Vec`s
    /// so it's not something to call on the hot path. Intended for
    /// era-boundary audits and manual dev-tools RPC checks. The
    /// underlying `KvdbMdbx::iter_range_owned` is snapshot-consistent
    /// per backend but the two snapshots aren't atomic against each
    /// other — take the report during a quiescent window (e.g.
    /// immediately after execution commit, before the next epoch's
    /// mutations).
    pub fn verify_parity(&self) -> Result<DualWriteReport> {
        let primary = self.primary.iter_range_owned(b"", None)?;
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
    #[cfg(test)]
    pub(crate) fn primary_for_test(&self) -> &KvdbMdbx {
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
    ) -> (tempdir::TempDir, tempdir::TempDir, MdbxShadowMirror) {
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
        assert_send_sync::<MdbxShadowMirror>();
        assert_send_sync::<Arc<MdbxShadowMirror>>();
    }
}
