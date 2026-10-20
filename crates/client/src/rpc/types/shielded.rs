// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::rpc::types::Bytes;
use mazze_types::{H256, U256, U64};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShieldedStatus {
    pub active: bool,
    pub activation_height: U64,
    pub slot_epochs: U64,
    pub max_txs_per_block: U64,
    pub tx_fee: U256,
    pub anchor_window_epochs: U64,
    pub chain_id: U64,
    /// The tree root after the latest executed epoch (48 bytes).
    pub root: Bytes,
    pub leaves: U64,
    /// The native balance backing every shielded note.
    pub backing: U256,
    pub pool_size: U64,
    pub executed_epoch: U64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShieldedSection {
    pub epoch: U64,
    pub block_hash: H256,
    pub transactions: Vec<ShieldedTxInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShieldedTxInfo {
    pub body_hash: H256,
    pub anchor: Bytes,
    pub nullifiers: [Bytes; 2],
    pub output_commitments: [Bytes; 2],
    pub recovery_tag: Bytes,
    /// The sealed recovery slot (64 bytes) and its tag (16 bytes): public,
    /// opened only by the wallet whose seed derives `recovery_tag`.
    pub recovery_slot: Bytes,
    pub recovery_mac: Bytes,
    pub v_public_in: U256,
    pub v_public_out: U256,
    pub fee: U256,
    pub applied: bool,
    /// Tree positions of the two outputs, when applied.
    pub positions: Option<[U64; 2]>,
    /// Why it was skipped, when skipped.
    pub skip_reason: Option<String>,
}
