// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::{
    block_data_manager::BlockDataManager,
    consensus::{
        BestInformation, ConsensusConfig, TransactionInfo, TrustedCheckpoint,
    },
    rpc_errors::Result as RpcResult,
    statistics::SharedStatistics,
    transaction_pool::SharedTransactionPool,
    ConsensusGraph,
};
use mazze_statedb::StateDb;
use mazze_storage::StorageState;
use mazze_types::{AllChainID, H256, U256};
use primitives::{EpochId, EpochNumber, SignedTransaction};
use std::{any::Any, collections::HashSet, sync::Arc};

/// FIXME: redesign this trait
pub trait ConsensusGraphTrait: Send + Sync {
    type ConsensusConfig;

    fn as_any(&self) -> &dyn Any;

    fn to_arc_consensus(self: Arc<Self>) -> Arc<ConsensusGraph>;

    fn get_config(&self) -> &Self::ConsensusConfig;

    fn on_new_block(&self, hash: &H256);

    fn update_total_weight_delta_heartbeat(&self) {}

    fn expected_difficulty(&self, parent_hash: &H256) -> U256;

    fn construct_main_state(&self);

    fn best_info(&self) -> Arc<BestInformation>;

    fn best_epoch_number(&self) -> u64;

    fn latest_checkpoint_epoch_number(&self) -> u64;

    fn latest_confirmed_epoch_number(&self) -> u64;

    fn best_chain_id(&self) -> AllChainID;

    fn best_block_hash(&self) -> H256;

    fn current_era_genesis_seq_num(&self) -> u64;

    fn get_data_manager(&self) -> &Arc<BlockDataManager>;

    fn get_tx_pool(&self) -> &SharedTransactionPool;

    fn get_statistics(&self) -> &SharedStatistics;

    fn block_count(&self) -> u64;

    fn get_hash_from_epoch_number(
        &self, epoch_number: EpochNumber,
    ) -> Result<H256, String>;

    fn get_block_hashes_by_epoch(
        &self, epoch_number: EpochNumber,
    ) -> Result<Vec<H256>, String>;

    fn get_skipped_block_hashes_by_epoch(
        &self, epoch_number: EpochNumber,
    ) -> Result<Vec<H256>, String>;

    /// Return transaction info for clients.
    /// Note that the skipped transactions (status == 2) will not be returned.
    fn get_transaction_info_by_hash(
        &self, hash: &H256,
    ) -> Option<(SignedTransaction, TransactionInfo)>;

    fn get_block_epoch_number(&self, hash: &H256) -> Option<u64>;

    fn get_block_number(
        &self, block_hash: &H256,
    ) -> Result<Option<u64>, String>;

    fn get_trusted_blame_block_for_snapshot(
        &self, snapshot_epoch_id: &EpochId,
    ) -> Option<H256>;

    fn get_to_sync_epoch_id(&self) -> EpochId;

    /// All configured fast-sync anchors, sorted newest-first.
    fn trusted_checkpoints(&self) -> Vec<TrustedCheckpoint> { Vec::new() }

    /// True while a fresh node with configured fast-sync anchors still has
    /// to anchor its consensus graph at one of them (see
    /// docs/fast-sync-design.md §5.19). False once anchored, for a node that
    /// already holds chain data, and for a node without anchors.
    fn fast_sync_bootstrap_pending(&self) -> bool { false }

    /// All configured fast-sync anchors as `(checkpoint_height,
    /// checkpoint_hash)`, sorted newest-first. The catch-up FSM proposes
    /// every one as a snapshot-sync candidate and syncs the newest a peer
    /// still serves, so a single stale anchor can't wedge the join.
    /// See docs/fast-sync-design.md §5.16.
    fn trusted_checkpoint_candidates(&self) -> Vec<(u64, H256)> { Vec::new() }

    fn get_trusted_blame_block(&self, stable_hash: &H256) -> Option<H256>;

    fn set_initial_sequence_number(&self, initial_sn: u64);

    fn get_state_db_by_epoch_number(
        &self, epoch_number: EpochNumber, rpc_param_name: &str,
    ) -> RpcResult<StateDb>;

    fn get_eth_state_db_by_epoch_number(
        &self, epoch_number: EpochNumber, rpc_param_name: &str,
    ) -> RpcResult<StateDb>;

    fn get_storage_state_by_epoch_number(
        &self, epoch_number: EpochNumber, rpc_param_name: &str,
    ) -> RpcResult<StorageState>;

    fn get_blocks_needing_bodies(&self) -> HashSet<H256>;

    /// Whether `hash` is a block of the in-memory consensus graph (the
    /// current era subtree). Unlike `get_block_epoch_number`, this never
    /// falls back to the DB.
    fn contains_block(&self, hash: &H256) -> bool;

    fn catch_up_completed(&self, peer_median_epoch: u64) -> bool;

    /// Number of epochs enqueued for execution but not yet executed.
    /// The sync layer throttles block downloads when this grows too
    /// large, so a node catching up can't pile unexecuted epochs (each
    /// pinning `Arc<Block>`s) into memory faster than the single
    /// execution thread can drain them. See the download↔execution
    /// backpressure fix.
    fn pending_execution_count(&self) -> usize;

    /// Height of the latest epoch whose state has actually been EXECUTED
    /// (the upper bound of the state-availability boundary). Unlike
    /// `pending_execution_count` (a queue depth that drains to ~0 when
    /// execution is *stuck* rather than merely slow), `best_epoch_number()
    /// - best_executed_state_epoch_number()` is the true header-vs-execution
    /// lag. The mining backpressure keys on this so a miner never races its
    /// header tip arbitrarily far ahead of executed state.
    fn best_executed_state_epoch_number(&self) -> u64;

    fn enter_normal_phase(&self);
    fn leave_normal_phase(&self);
    fn reset(&self);
}

pub type SharedConsensusGraph =
    Arc<dyn ConsensusGraphTrait<ConsensusConfig = ConsensusConfig>>;
