// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use mazze_types::{H256, U64};
use serde_derive::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainProgress {
    /// Best pivot-chain epoch number.
    pub best_epoch_number: U64,
    /// Best pivot-chain block number.
    pub best_block_number: U64,
    /// Total number of processed blocks seen by the node.
    pub processed_block_count: U64,
    /// Latest checkpoint epoch number.
    pub latest_checkpoint_epoch_number: U64,
    /// Latest confirmed epoch number.
    pub latest_confirmed_epoch_number: U64,
    /// Latest executed state epoch number.
    pub latest_state_epoch_number: U64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RandomXProgress {
    /// Current RandomX epoch index derived from the best epoch number.
    pub epoch_number: U64,
    /// Number of Mazze epochs covered by one RandomX epoch.
    pub epoch_length: U64,
    /// First Mazze epoch number included in the current RandomX epoch.
    pub start_epoch_number: U64,
    /// Last Mazze epoch number included in the current RandomX epoch.
    pub end_epoch_number: U64,
    /// Mazze epoch number where the next RandomX epoch begins.
    pub next_transition_epoch_number: U64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EraProgress {
    /// Current era index.
    pub number: U64,
    /// Number of epochs in one era.
    pub epoch_count: U64,
    /// First epoch number of the current era.
    pub start_epoch_number: U64,
    /// Last epoch number of the current era.
    pub end_epoch_number: U64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotProgress {
    /// Number of epochs between snapshot boundaries.
    pub epoch_length: U64,
    /// Latest locally available snapshot epoch number.
    pub latest_snapshot_epoch_number: U64,
    /// Number of locally available snapshots that can be served.
    pub available_snapshot_count: U64,
    /// Whether the node currently has at least one snapshot to serve.
    pub serving: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// Hash of the block
    pub best_hash: H256,
    /// The best chain id,
    pub chain_id: U64,
    /// The best chain id,
    pub ethereum_space_chain_id: U64,
    /// The network id,
    pub network_id: U64,
    /// Legacy alias for `progress.best_epoch_number`.
    pub epoch_number: U64,
    /// Legacy alias for `progress.processed_block_count`.
    pub block_number: U64,
    /// The number of pending transactions
    pub pending_tx_number: U64,
    /// Legacy alias for `progress.latest_checkpoint_epoch_number`.
    pub latest_checkpoint: U64,
    /// Legacy alias for `progress.latest_confirmed_epoch_number`.
    pub latest_confirmed: U64,
    /// Legacy alias for `progress.latest_state_epoch_number`.
    pub latest_state: U64,
    /// Explicit chain progress fields intended for dashboards and operators.
    pub progress: ChainProgress,
    /// Explicit RandomX epoch fields derived from the current best epoch.
    pub randomx: RandomXProgress,
    /// Snapshot production and serving information for fast bootstrap.
    pub snapshots: SnapshotProgress,
    /// Explicit era fields for the current consensus era.
    pub era: EraProgress,
}
