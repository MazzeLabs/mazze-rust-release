// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use mazze_types::{H256, U64};
use serde_derive::Serialize;

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
    /// The number of epochs
    pub epoch_number: U64,
    /// The number of blocks
    pub block_number: U64,
    /// The number of pending transactions
    pub pending_tx_number: U64,
    /// The latest checkpoint epoch.
    pub latest_checkpoint: U64,
    /// The latest confirmed epoch.
    pub latest_confirmed: U64,
    /// The latest executed epoch.
    pub latest_state: U64,
}
