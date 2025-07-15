// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::consensus::{
    consensus_inner::{ConsensusGraphInner},
    consensus_inner::consensus_executor::{ConsensusExecutor, EpochExecutionTask},
    consensus_trait::ConsensusGraphTrait,
    ConsensusGraph,
};
use mazze_internal_common::{debug::ComputeEpochDebugRecord, StateRootWithAuxInfo};
use mazze_types::H256;
use serde_json;
use std::{fs::File, io::Write, path::Path};

#[allow(dead_code)]
pub enum RecomputeResult {
    BlockNotFound,
    OK {
        state_root_from_recompute: StateRootWithAuxInfo,
        receipts_root_from_recompute: H256,
        logs_bloom_hash_from_recompute: H256,
        state_root_from_consensus: StateRootWithAuxInfo,
        receipts_root_from_consensus: H256,
        logs_bloom_hash_from_consensus: H256,
        debug_output: Box<ComputeEpochDebugRecord>,
    },
}

pub fn debug_recompute_epoch(
    consensus: &ConsensusGraph, epoch_hash: H256,
) -> RecomputeResult {
    let mut debug_record = Box::new(ComputeEpochDebugRecord::default());

    let inner_read_guard = consensus.inner.read();
    let me = if let Some(me) = inner_read_guard.hash_to_arena_indices.get(&epoch_hash) {
        *me
    } else {
        return RecomputeResult::BlockNotFound;
    };

    let (
        main_block_header,
        parent_hash,
        state_root_from_consensus,
        receipts_root_from_consensus,
        logs_bloom_hash_from_consensus,
    ) = {
        let main_block_header = inner_read_guard
            .data_man
            .block_header_by_hash(&inner_read_guard.arena[me].hash)
            .expect("Main block header must exist in data manager")
            .as_ref()
            .clone();
        let parent_hash = *main_block_header.parent_hash();
        let commitment = inner_read_guard
            .data_man
            .get_epoch_execution_commitment(&epoch_hash)
            .unwrap();

        (
            main_block_header,
            parent_hash,
            commitment.state_root_with_aux_info.clone(),
            commitment.receipts_root,
            commitment.logs_bloom_hash,
        )
    };

    drop(inner_read_guard); // Release the read lock before acquiring write lock for reward_info

    let reward_info = consensus.executor.get_reward_execution_info(
        &mut consensus.inner.write(),
        me,
    );

    debug_record.block_hash = epoch_hash;
    debug_record.block_height = main_block_header.height();
    debug_record.parent_epoch_hash = parent_hash;
    debug_record.parent_state_root = consensus
        .get_data_manager()
        .get_epoch_execution_commitment(&parent_hash)
        .unwrap()
        .state_root_with_aux_info.clone();

    let inner_read_guard_for_compute_epoch = consensus.inner.read();

    consensus.executor.compute_epoch(
        EpochExecutionTask::new(
            me,
            &*inner_read_guard_for_compute_epoch,
            reward_info,
            true,  /* on_local_main */
            true,  /* force_recompute */
        ),
        Some(&mut debug_record),
        true,
    );

    RecomputeResult::OK {
        state_root_from_recompute: debug_record.parent_state_root.clone(),
        receipts_root_from_recompute: Default::default(),
        logs_bloom_hash_from_recompute: Default::default(),
        state_root_from_consensus,
        receipts_root_from_consensus,
        logs_bloom_hash_from_consensus,
        debug_output: debug_record,
    }
}

pub fn log_debug_epoch_computation(
    epoch_arena_index: usize, inner: &mut ConsensusGraphInner,
    executor: &ConsensusExecutor, _block_hash: H256, block_height: u64,
) -> ComputeEpochDebugRecord {
    // Parent state root.
    let parent_arena_index = inner.arena[epoch_arena_index].parent;
    let parent_epoch_hash = inner.arena[parent_arena_index].hash;
    let parent_state_root = inner
        .data_man
        .get_epoch_execution_commitment(&parent_epoch_hash)
        .unwrap()
        .state_root_with_aux_info
        .clone();

    let reward_index = inner.get_main_reward_index(epoch_arena_index);

    let reward_execution_info =
        executor.get_reward_execution_info_from_index(inner, reward_index);
    let task = EpochExecutionTask::new(
        epoch_arena_index,
        inner,
        reward_execution_info,
        false, /* on_local_main */
        false, /* force_recompute */
    );
    let mut debug_record = ComputeEpochDebugRecord::default();
    {
        debug_record.block_height = block_height;
        debug_record.block_hash = _block_hash;
        debug_record.parent_epoch_hash = parent_epoch_hash;
        debug_record.parent_state_root = parent_state_root;
        
        let blocks =
            inner.get_epoch_block_hashes(epoch_arena_index)
            .iter()
            .map(|hash| {
                inner
                    .data_man
                    .block_by_hash(hash, false /* update_cache */)
                    .unwrap()
            })
            .collect::<Vec<_>>();

        debug_record.block_txs = blocks
            .iter()
            .map(|block| block.transactions.len())
            .collect::<Vec<_>>();
        debug_record.transactions = blocks
            .iter()
            .flat_map(|block| block.transactions.clone())
            .collect::<Vec<_>>();
    }
    executor.compute_epoch(task, Some(&mut debug_record), false);

    debug_record
}

pub fn log_invalid_state_root(
    deferred: usize, inner: &mut ConsensusGraphInner,
    executor: &ConsensusExecutor, block_hash: H256, block_height: u64,
    _state_root: &StateRootWithAuxInfo,
) -> std::io::Result<()> {
    if let Some(dump_dir) =
        inner.inner_conf.debug_dump_dir_invalid_state_root.clone()
    {
        let invalid_state_root_path =
            dump_dir.clone() + &format!("{}_{:?}", block_height, block_hash);
        let txt_path = invalid_state_root_path.clone() + ".txt";
        if Path::new(&txt_path).exists() {
            return Ok(());
        }

        // TODO: refactor the consensus executor to make it run at background.
        let debug_record = log_debug_epoch_computation(
            deferred,
            inner,
            executor,
            block_hash,
            block_height,
        );
        let deferred_block_hash = inner.arena[deferred].hash;
        let got_state_root = inner
            .data_man
            .get_epoch_execution_commitment(&deferred_block_hash)
            .unwrap()
            .state_root_with_aux_info
            .clone();

        {
            std::fs::create_dir_all(dump_dir)?;

            let mut debug_file = File::create(&txt_path)?;
            debug_file.write_all(format!("{:?}", debug_record).as_bytes())?;
            let json_path = invalid_state_root_path + ".json.txt";
            let mut json_file = File::create(&json_path)?;
            json_file
                .write_all(serde_json::to_string(&debug_record)?.as_bytes())?;
        }

        warn!(
            "State debug recompute: got {:?}, deferred block: {:?}, block hash: {:?}\
             number of transactions in epoch: {:?}, rewards: {:?}",
            got_state_root,
            deferred_block_hash,
            block_hash,
            debug_record.transactions.len(),
            debug_record.merged_rewards_by_author,
        );
    }

    Ok(())
}
