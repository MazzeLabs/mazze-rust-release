// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! JSON-serializable version of `mazze_storage::DualWriteReport`
//! for the `debug_mdbxShadowVerifyParity` RPC.
//!
//! The core storage type carries `Box<[u8]>` keys — not directly
//! JSON-serializable. This wrapper hex-encodes each divergent key
//! (`0x`-prefixed) so operator dashboards can eyeball / grep the
//! output without decoding.

/// Report emitted by `debug_mdbxShadowVerifyParity` for a single
/// shadow-mirrored column.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MdbxShadowParityReport {
    /// Human-readable column name (`"HashByNumber"`, etc.).
    pub table: String,
    /// Number of entries observed in the primary (ParityDB) column.
    pub primary_count: usize,
    /// Number of entries observed in the shadow (MDBX) column.
    pub shadow_count: usize,
    /// Hex-encoded keys present in the primary but absent from the
    /// shadow. `0x`-prefixed lowercase.
    pub missing_in_shadow: Vec<String>,
    /// Hex-encoded keys present in the shadow but absent from the
    /// primary. `0x`-prefixed lowercase.
    pub extra_in_shadow: Vec<String>,
    /// Hex-encoded keys present on both sides but with different
    /// values. `0x`-prefixed lowercase.
    pub value_mismatches: Vec<String>,
    /// Convenience flag: `true` when no divergence is observed and
    /// primary_count == shadow_count. When this is `true` for N
    /// consecutive era boundaries, Phase 3 can flip reads to the
    /// shadow.
    pub is_matched: bool,
    /// `missing_in_shadow.len() + extra_in_shadow.len() +
    ///  value_mismatches.len()`, hoisted so dashboards can alert on
    /// a single number without inspecting the arrays.
    pub diverged_count: usize,
}

impl MdbxShadowParityReport {
    /// Build the RPC response from the storage-layer report. Hex-
    /// encodes each divergent key as a `0x`-prefixed lowercase
    /// string.
    pub fn from_storage_report(
        report: mazze_storage::DualWriteReport,
    ) -> Self {
        fn hex(k: &[u8]) -> String {
            format!("0x{}", rustc_hex::ToHex::to_hex::<String>(k))
        }
        Self {
            table: report.table.to_string(),
            primary_count: report.primary_count,
            shadow_count: report.shadow_count,
            is_matched: report.is_matched(),
            diverged_count: report.diverged_count(),
            missing_in_shadow: report
                .missing_in_shadow
                .iter()
                .map(|k| hex(k))
                .collect(),
            extra_in_shadow: report
                .extra_in_shadow
                .iter()
                .map(|k| hex(k))
                .collect(),
            value_mismatches: report
                .value_mismatches
                .iter()
                .map(|k| hex(k))
                .collect(),
        }
    }
}
