// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

#[derive(Debug, Serialize, Deserialize)]
pub struct BlockHashAuthorValue<ValueType>(
    pub H256,
    pub Address,
    pub ValueType,
);

//#[derive(Debug, Serialize, Deserialize)]
//pub struct BlockHashValue<ValueType>(pub H256, pub ValueType);

#[derive(Debug, Serialize, Deserialize)]
pub struct AuthorValue<ValueType>(pub Address, pub ValueType);

#[derive(Debug, Serialize, Deserialize)]
pub struct ComputeEpochDebugRecord {
    // Basic information.
    pub block_height: u64,
    pub block_hash: H256,
    pub parent_epoch_hash: H256,
    pub parent_state_root: StateRootWithAuxInfo,

    // Blocks.
    pub block_hashes: Vec<H256>,
    pub block_txs: Vec<usize>,
    pub transactions: Vec<Arc<SignedTransaction>>,
    
    // Storage operations.
    // op name, key, maybe_value
    pub state_ops: Vec<StateOp>,
    
    // For reward distribution
    pub merged_rewards_by_author: Vec<AuthorValue<U256>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum StateOp {
    IncentiveLevelOp {
        op_name: String,
        key: Vec<u8>,
        maybe_value: Option<Vec<u8>>,
    },
    StorageLevelOp {
        op_name: String,
        key: Vec<u8>,
        maybe_value: Option<Vec<u8>>,
    },
}

impl Default for ComputeEpochDebugRecord {
    fn default() -> Self {
        Self {
            block_hash: Default::default(),
            block_height: 0,
            parent_epoch_hash: Default::default(),
            parent_state_root: StateRootWithAuxInfo::genesis(
                &Default::default(),
            ),
            block_hashes: Default::default(),
            block_txs: Default::default(),
            transactions: Default::default(),
            state_ops: Default::default(),
            merged_rewards_by_author: Default::default(),
        }
    }
}

use crate::StateRootWithAuxInfo;
use mazze_types::{Address, H256, U256};
use primitives::SignedTransaction;
use serde_derive::{Deserialize, Serialize};
use std::{sync::Arc, vec::Vec};
