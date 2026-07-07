// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Prefix layout for the Phase 5c snapshot MDBX column.
//!
//! Every snapshot key inside the dedicated snapshot env
//! ([`SnapshotMdbxConfig`](crate::SnapshotMdbxConfig)) is 33 bytes
//! of prefix + a variable-length inner key:
//!
//! ```text
//! [ snapshot_epoch_id (32B) ] [ sub_prefix (1B) ] [ inner_key (varlen) ]
//! ```
//!
//! - The **32-byte snapshot epoch id** isolates every snapshot's
//!   data into a contiguous MDBX B+tree slice (the same trick 4c
//!   uses for delta MPTs, see
//!   [`prefixed_kvdb_mdbx`](super::prefixed_kvdb_mdbx)).
//! - The **1-byte `sub_prefix`** further isolates the four data
//!   flavours a snapshot carries (KV state / delta set / delta del
//!   / MPT nodes) plus the reserved crash-recovery marker. Values
//!   are carried over verbatim from the ParityDB reference impl
//!   (`snapshot_kv_db_paritydb.rs:44-47`) so operator log lines
//!   and dashboards stay greppable through the cutover.
//! - The **marker** `0x21` (`b'!'`) sorts before every data
//!   sub_prefix (`0x64` `b'd'` ≤ `0x6b` `b'k'` ≤ `0x6d` `b'm'` ≤
//!   `0x73` `b's'`), so a `first_in_range()` probe on a snapshot's
//!   32-byte slice returns the marker first when a crashed merge
//!   left one behind — cheap detection for `scan_persist_state`.
//!
//! See `docs/internal/storage-phase-5-cde-migration.md` §2.3.0
//! (layout) and §2.3.2.1 (marker protocol).

use primitives::EpochId;

/// Length of the snapshot-scoping prefix (epoch id) portion.
pub const SNAPSHOT_EPOCH_ID_LEN: usize = 32;

/// Length of the sub-prefix (KV / SET / DEL / MPT / marker) byte.
pub const SUB_PREFIX_LEN: usize = 1;

/// Total length of a snapshot key's prefix — `[epoch_id | sub]`.
pub const SNAPSHOT_PREFIX_LEN: usize =
    SNAPSHOT_EPOCH_ID_LEN + SUB_PREFIX_LEN;

/// Flat state key/value pairs. Every account and every storage
/// slot lives under this sub_prefix for the snapshot's generation.
pub const SUB_PREFIX_KV: u8 = b'k';

/// Delta MPT's **set** dump — keys the delta writes with a
/// non-empty value. Consumed by `direct_merge` / `copy_and_merge`.
pub const SUB_PREFIX_DELTA_SET: u8 = b's';

/// Delta MPT's **delete** dump — presence-only; the value payload
/// is empty. Kept as a separate sub-table for clear iteration
/// semantics and to preserve the paritydb symmetry.
pub const SUB_PREFIX_DELTA_DEL: u8 = b'd';

/// Snapshot MPT nodes. Populated during `direct_merge` /
/// `copy_and_merge`; the resulting merkle root is the child
/// snapshot's identity.
pub const SUB_PREFIX_MPT: u8 = b'm';

/// Reserved crash-recovery marker. Written by the FIRST rw_txn of a
/// chunked merge or full-sync ingest, deleted by the LAST rw_txn.
/// Its byte value (`0x21`, `b'!'`) sorts strictly below every data
/// sub_prefix so a `first_in_range()` probe on the snapshot's
/// 32-byte slice sees the marker first when a crash left one
/// behind — see `scan_persist_state` in the 5c manager.
///
/// **Invariant**: must sort strictly below all `SUB_PREFIX_*` data
/// bytes. Enforced by [`marker_sorts_first`](tests::marker_sorts_first).
pub const SUB_PREFIX_MERGE_MARKER: u8 = b'!';

/// Kinds of crash-recovery markers the manager can write. Encoded
/// as one u8 inside the marker value (which is otherwise an RLP
/// blob owned by the 5c.d commit — this module only names the
/// tag). Kept as an enum here so `SnapshotDbManagerMdbx` and its
/// tests can name each variant without pulling the full marker
/// struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MergeMarkerKind {
    /// A `new_snapshot_by_merging` in flight — the marker's RLP
    /// carries `parent_epoch_id` + `started_at` so
    /// `scan_persist_state` can log the origin and range-delete the
    /// partial child.
    Merge = 1,
    /// A `new_temp_snapshot_for_full_sync` in flight — the RLP
    /// carries `merkle_root` for the same GC path.
    FullSync = 2,
}

impl MergeMarkerKind {
    /// Byte tag for RLP encoding. Kept as a small u8 so callers can
    /// switch on the raw byte in the manager's marker parser.
    pub fn tag(self) -> u8 { self as u8 }

    /// Parse the tag byte back to a variant. `None` on an unknown
    /// tag — treat as "unknown marker, delete the partial snapshot
    /// but do not attempt resumption".
    pub fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Merge),
            2 => Some(Self::FullSync),
            _ => None,
        }
    }
}

/// Compose the 32-byte snapshot slice prefix for `epoch_id`. Used
/// by `first_in_range` probes and by `destroy_snapshot` to bound a
/// chunked range delete over the whole snapshot without needing to
/// know the sub_prefix layout.
pub fn compose_snapshot_scope(epoch_id: &EpochId) -> [u8; SNAPSHOT_EPOCH_ID_LEN] {
    let mut out = [0u8; SNAPSHOT_EPOCH_ID_LEN];
    out.copy_from_slice(epoch_id.as_ref());
    out
}

/// Compose the 33-byte prefix for the given `(epoch_id, sub)`
/// slice. This is the prefix `PrefixedKvdbMdbx` uses to scope all
/// reads/writes for one sub-table of one snapshot.
///
/// **Length invariant**: the returned `Vec` is exactly
/// [`SNAPSHOT_PREFIX_LEN`] bytes so callers can assert it before
/// handing to the primitive.
pub fn compose_snapshot_prefix(
    epoch_id: &EpochId, sub_prefix: u8,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(SNAPSHOT_PREFIX_LEN);
    out.extend_from_slice(epoch_id.as_ref());
    out.push(sub_prefix);
    debug_assert_eq!(out.len(), SNAPSHOT_PREFIX_LEN);
    out
}

/// The exclusive upper bound of `epoch_id`'s 32-byte slice — the
/// smallest 32-byte value strictly greater than every possible key
/// within the snapshot. Returns `None` when `epoch_id` is
/// `0xff…ff` (there is no representable next value).
///
/// Consumed by chunked `destroy_snapshot` and by
/// `scan_persist_state`'s prefix-enumeration jump.
pub fn snapshot_scope_upper_bound(
    epoch_id: &EpochId,
) -> Option<[u8; SNAPSHOT_EPOCH_ID_LEN]> {
    let mut ub = compose_snapshot_scope(epoch_id);
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

// ---------------------------- tests ----------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The marker byte MUST sort strictly below every data
    /// sub_prefix — that is what makes `first_in_range` on a
    /// snapshot's 32-byte slice a valid crash-detector.
    #[test]
    fn marker_sorts_first() {
        for data_sub in [
            SUB_PREFIX_KV,
            SUB_PREFIX_DELTA_SET,
            SUB_PREFIX_DELTA_DEL,
            SUB_PREFIX_MPT,
        ] {
            assert!(
                SUB_PREFIX_MERGE_MARKER < data_sub,
                "marker byte 0x{:02x} must sort below data sub_prefix \
                 0x{:02x} for the scan_persist_state crash detector",
                SUB_PREFIX_MERGE_MARKER,
                data_sub,
            );
        }
    }

    /// All sub_prefix bytes must be distinct — a collision would
    /// mean two data flavours share a slice and reads/writes for
    /// one would silently mangle the other.
    #[test]
    fn sub_prefixes_are_distinct() {
        let all = [
            SUB_PREFIX_KV,
            SUB_PREFIX_DELTA_SET,
            SUB_PREFIX_DELTA_DEL,
            SUB_PREFIX_MPT,
            SUB_PREFIX_MERGE_MARKER,
        ];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(
                    all[i], all[j],
                    "sub_prefix collision between {} and {}",
                    i, j
                );
            }
        }
    }

    /// Byte values match the paritydb reference — this is a
    /// deliberate operator-facing contract (log lines /
    /// dashboards). See file-level doc.
    #[test]
    fn sub_prefixes_match_paritydb_reference() {
        assert_eq!(SUB_PREFIX_KV, b'k');
        assert_eq!(SUB_PREFIX_DELTA_SET, b's');
        assert_eq!(SUB_PREFIX_DELTA_DEL, b'd');
        assert_eq!(SUB_PREFIX_MPT, b'm');
    }

    /// `compose_snapshot_prefix` produces exactly 33 bytes and
    /// preserves the `[epoch_id | sub]` layout byte-for-byte.
    #[test]
    fn compose_layout_is_epoch_then_sub() {
        let epoch = EpochId::from_slice(&[0x42u8; SNAPSHOT_EPOCH_ID_LEN]);
        let prefix = compose_snapshot_prefix(&epoch, SUB_PREFIX_KV);
        assert_eq!(prefix.len(), SNAPSHOT_PREFIX_LEN);
        assert_eq!(&prefix[..SNAPSHOT_EPOCH_ID_LEN], epoch.as_ref());
        assert_eq!(prefix[SNAPSHOT_EPOCH_ID_LEN], SUB_PREFIX_KV);
    }

    /// Composing with two different sub_prefixes for the same
    /// epoch id yields prefixes that share the first 32 bytes and
    /// differ only in byte 32.
    #[test]
    fn compose_sub_prefixes_isolate() {
        let epoch = EpochId::from_slice(&[0x0a; SNAPSHOT_EPOCH_ID_LEN]);
        let kv = compose_snapshot_prefix(&epoch, SUB_PREFIX_KV);
        let mpt = compose_snapshot_prefix(&epoch, SUB_PREFIX_MPT);
        assert_eq!(&kv[..SNAPSHOT_EPOCH_ID_LEN], &mpt[..SNAPSHOT_EPOCH_ID_LEN]);
        assert_ne!(kv[SNAPSHOT_EPOCH_ID_LEN], mpt[SNAPSHOT_EPOCH_ID_LEN]);
    }

    /// `snapshot_scope_upper_bound` returns the numerically-next
    /// 32-byte value; `None` when the input is `0xff…ff`.
    #[test]
    fn scope_upper_bound_shape() {
        let low = EpochId::from_slice(&[0u8; SNAPSHOT_EPOCH_ID_LEN]);
        let mut expected = [0u8; SNAPSHOT_EPOCH_ID_LEN];
        expected[SNAPSHOT_EPOCH_ID_LEN - 1] = 1;
        assert_eq!(snapshot_scope_upper_bound(&low), Some(expected));

        let high = EpochId::from_slice(&[0xff; SNAPSHOT_EPOCH_ID_LEN]);
        assert_eq!(snapshot_scope_upper_bound(&high), None);
    }

    /// `snapshot_scope_upper_bound` correctly carries across byte
    /// boundaries: `[0x00, 0xff]` → `[0x01, 0x00]`.
    #[test]
    fn scope_upper_bound_carries() {
        let mut prefix = [0u8; SNAPSHOT_EPOCH_ID_LEN];
        prefix[SNAPSHOT_EPOCH_ID_LEN - 1] = 0xff;
        let epoch = EpochId::from_slice(&prefix);
        let mut expected = [0u8; SNAPSHOT_EPOCH_ID_LEN];
        expected[SNAPSHOT_EPOCH_ID_LEN - 2] = 1;
        assert_eq!(snapshot_scope_upper_bound(&epoch), Some(expected));
    }

    /// `MergeMarkerKind` round-trips through its byte tag.
    #[test]
    fn merge_marker_kind_round_trips() {
        for variant in [MergeMarkerKind::Merge, MergeMarkerKind::FullSync] {
            assert_eq!(MergeMarkerKind::from_tag(variant.tag()), Some(variant));
        }
        // Unknown tags return None.
        assert_eq!(MergeMarkerKind::from_tag(0), None);
        assert_eq!(MergeMarkerKind::from_tag(255), None);
    }
}
