// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use super::blame_verifier::BlameVerifier;
use crate::{
    block_data_manager::{BlockDataManager, BlockStatus, LocalBlockInfo},
    channel::Channel,
    consensus::{
        consensus_inner::{
            confirmation_meter::ConfirmationMeter,
            consensus_executor::{ConsensusExecutor, EpochExecutionTask},
            ConsensusGraphInner, NULL,
        },
        ConsensusConfig,
    },
    state_exposer::{ConsensusGraphBlockState, STATE_EXPOSER},
    statistics::SharedStatistics,
    NodeType, Notifications, SharedTransactionPool,
};
use hibitset::{BitSet, BitSetLike, DrainableBitSet};
use mazze_parameters::{consensus::*, consensus_internal::*};
use mazze_storage::{
    StateIndex,
};
use mazze_types::H256;
use parking_lot::Mutex;
use primitives::{MERKLE_NULL_NODE};
use std::{
    cmp::{max, min},
    collections::{HashMap, HashSet, VecDeque},
    slice::Iter,
    sync::Arc,
};

// TODO: Remove this
#[allow(dead_code)]
pub struct ConsensusNewBlockHandler {
    conf: ConsensusConfig,
    txpool: SharedTransactionPool,
    data_man: Arc<BlockDataManager>,
    executor: Arc<ConsensusExecutor>,
    statistics: SharedStatistics,

    /// Channel used to send epochs to PubSub
    /// Each element is <epoch_number, epoch_hashes>
    epochs_sender: Arc<Channel<(u64, Vec<H256>)>>,

    /// API used for verifying blaming on light nodes.
    blame_verifier: Mutex<BlameVerifier>,

    /// The type of this node: Archive, Full, or Light.
    node_type: NodeType,
}

/// ConsensusNewBlockHandler contains all sub-routines for handling new arriving
/// blocks from network or db. It manipulates and updates ConsensusGraphInner
/// object accordingly.
impl ConsensusNewBlockHandler {
    pub fn new(
        conf: ConsensusConfig,
        txpool: SharedTransactionPool,
        data_man: Arc<BlockDataManager>,
        executor: Arc<ConsensusExecutor>,
        statistics: SharedStatistics,
        notifications: Arc<Notifications>,
        node_type: NodeType,
    ) -> Self {
        let epochs_sender = notifications.epochs_ordered.clone();
        let blame_verifier =
            Mutex::new(BlameVerifier::new(data_man.clone(), notifications));

        Self {
            conf,
            txpool,
            data_man,
            executor,
            statistics,
            epochs_sender,
            blame_verifier,
            node_type,
        }
    }

    /// Return (old_era_block_set, new_era_block_set).
    /// `old_era_block_set` includes the blocks in the past of
    /// `new_era_block_arena_index`. `new_era_block_set` includes all other
    /// blocks (the outlier and the future).
    fn compute_old_era_and_new_era_block_set(
        inner: &mut ConsensusGraphInner, new_era_block_arena_index: usize,
    ) -> (HashSet<usize>, HashSet<usize>) {
        // We first compute the set of blocks inside the new era and we
        // recompute the past_weight inside the stable height.
        let mut old_era_block_arena_index_set = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(new_era_block_arena_index);
        while let Some(x) = queue.pop_front() {
            if inner.arena[x].parent != NULL
                && !old_era_block_arena_index_set
                    .contains(&inner.arena[x].parent)
            {
                old_era_block_arena_index_set.insert(inner.arena[x].parent);
                queue.push_back(inner.arena[x].parent);
            }
            for referee in &inner.arena[x].referees {
                if *referee != NULL
                    && !old_era_block_arena_index_set.contains(referee)
                {
                    old_era_block_arena_index_set.insert(*referee);
                    queue.push_back(*referee);
                }
            }
        }
        let mut new_era_block_arena_index_set = HashSet::new();
        for (i, _) in &inner.arena {
            if !old_era_block_arena_index_set.contains(&i) {
                new_era_block_arena_index_set.insert(i);
            }
        }
        (old_era_block_arena_index_set, new_era_block_arena_index_set)
    }

    /// Note that there is an important assumption: the timer chain must have no
    /// block in the outlier of new_era_block_arena_index. If this is not
    /// true, it cannot become a checkpoint block
    fn make_checkpoint_at(
        inner: &mut ConsensusGraphInner, new_era_block_arena_index: usize,
    ) {
        let new_era_height = inner.arena[new_era_block_arena_index].height;
        let (outside_block_arena_indices, new_era_block_arena_index_set) =
            Self::compute_old_era_and_new_era_block_set(
                inner,
                new_era_block_arena_index,
            );

        // This is the arena indices for legacy blocks.
        let mut new_era_genesis_subtree = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(new_era_block_arena_index);
        while let Some(x) = queue.pop_front() {
            new_era_genesis_subtree.insert(x);
            for child in &inner.arena[x].children {
                queue.push_back(*child);
            }
        }
        let new_era_legacy_block_arena_index_set: HashSet<_> =
            new_era_block_arena_index_set
                .difference(&new_era_genesis_subtree)
                .collect();

        // Next we are going to recompute all referee and referrer information
        // in arena
        let new_era_main_index = inner.height_to_main_index(new_era_height);
        for v in new_era_block_arena_index_set.iter() {
            let me = *v;
            // It is necessary to process `referees` and
            // `blockset_in_own_view_of_epoch` because
            // `new_era_block_arena_index_set` include the blocks in
            // the outlier of the new era genesis.
            inner.arena[me]
                .referees
                .retain(|v| new_era_block_arena_index_set.contains(v));
            inner.arena[me]
                .data
                .blockset_in_own_view_of_epoch
                .retain(|v| new_era_block_arena_index_set.contains(v));
            if !new_era_block_arena_index_set.contains(
                &inner.arena[me].data.past_view_last_timer_block_arena_index,
            ) {
                inner.arena[me].data.past_view_last_timer_block_arena_index =
                    NULL;
            }
            if !new_era_block_arena_index_set
                .contains(&inner.arena[me].data.force_confirm)
            {
                inner.arena[me].data.force_confirm = new_era_block_arena_index;
            }
        }
        // reassign the parent for outside era blocks
        for v in new_era_legacy_block_arena_index_set {
            let me = *v;
            let mut parent = inner.arena[me].parent;
            if inner.arena[me].era_block != NULL {
                inner.split_root(me);
            }
            if !new_era_block_arena_index_set.contains(&parent) {
                parent = NULL;
            }
            inner.arena[me].parent = parent;
            inner.arena[me].era_block = NULL;
            inner.terminal_hashes.remove(&inner.arena[me].hash);
        }
        // Now we are ready to cleanup outside blocks in inner data structures
        inner
            .pastset_cache
            .intersect_update(&outside_block_arena_indices);
        for index in outside_block_arena_indices {
            let hash = inner.arena[index].hash;
            inner.hash_to_arena_indices.remove(&hash);
            inner.terminal_hashes.remove(&hash);
            inner.arena.remove(index);
            // remove useless data in BlockDataManager
            inner.data_man.remove_epoch_execution_commitment(&hash);
            inner.data_man.remove_epoch_execution_context(&hash);
        }

        // Now we truncate the timer chain that are outside the genesis.
        let mut timer_chain_truncate = 0;
        while timer_chain_truncate < inner.timer_chain.len()
            && !new_era_block_arena_index_set
                .contains(&inner.timer_chain[timer_chain_truncate])
        {
            timer_chain_truncate += 1;
        }
        inner.cur_era_genesis_timer_chain_height += timer_chain_truncate as u64;
        assert_eq!(
            inner.cur_era_genesis_timer_chain_height,
            inner.arena[new_era_block_arena_index]
                .data
                .ledger_view_timer_chain_height
        );
        for i in 0..(inner.timer_chain.len() - timer_chain_truncate) {
            inner.timer_chain[i] = inner.timer_chain[i + timer_chain_truncate];
            if i + timer_chain_truncate
                < inner.timer_chain_accumulative_lca.len()
            {
                inner.timer_chain_accumulative_lca[i] = inner
                    .timer_chain_accumulative_lca[i + timer_chain_truncate];
            }
        }
        inner
            .timer_chain
            .resize(inner.timer_chain.len() - timer_chain_truncate, 0);
        if inner.timer_chain_accumulative_lca.len() > timer_chain_truncate {
            inner.timer_chain_accumulative_lca.resize(
                inner.timer_chain_accumulative_lca.len() - timer_chain_truncate,
                0,
            );
        } else {
            inner.timer_chain_accumulative_lca.clear();
        }
        // Move LCA to new genesis if necessary!
        for i in 0..inner.timer_chain_accumulative_lca.len() {
            if i < inner.inner_conf.timer_chain_beta as usize - 1
                || !new_era_genesis_subtree
                    .contains(&inner.timer_chain_accumulative_lca[i])
            {
                inner.timer_chain_accumulative_lca[i] =
                    new_era_block_arena_index;
            }
        }

        assert!(new_era_main_index < inner.main_chain.len());
        inner.main_chain = inner.main_chain.split_off(new_era_main_index);
        inner.main_chain_metadata =
            inner.main_chain_metadata.split_off(new_era_main_index);
        // Recompute past weight values
        inner.main_chain_metadata[0].past_weight =
            inner.block_weight(new_era_block_arena_index);
        for i in 1..inner.main_chain_metadata.len() {
            let main = inner.main_chain[i];
            inner.main_chain_metadata[i].past_weight =
                inner.main_chain_metadata[i - 1].past_weight
                    + inner.block_weight(main)
        }
        for d in inner.main_chain_metadata.iter_mut() {
            d.last_main_in_past_blocks
                .retain(|v| new_era_block_arena_index_set.contains(v));
        }
        inner
            .outlier_cache
            .intersect_update(&new_era_genesis_subtree);

        // Clear best_terminals_lca_caches
        inner.best_terminals_lca_height_cache.clear();

        // Clear has_timer_block_in_outlier cache
        inner.has_timer_block_in_outlier_cache.clear();

        // Chop off all link-cut-trees in the inner data structure
        inner.split_root(new_era_block_arena_index);

        inner.cur_era_genesis_block_arena_index = new_era_block_arena_index;
        inner.cur_era_genesis_height = new_era_height;

        let cur_era_hash = inner.arena[new_era_block_arena_index].hash.clone();
        let stable_era_arena_index =
            inner.get_main_block_arena_index(inner.cur_era_stable_height);
        let stable_era_hash = inner.arena[stable_era_arena_index].hash.clone();

        // This must be true given our checkpoint rule!
        for (_, x) in &inner.invalid_block_queue {
            assert!(new_era_block_arena_index_set.contains(x))
        }

        inner.data_man.set_cur_consensus_era_genesis_hash(
            &cur_era_hash,
            &stable_era_hash,
        );
        inner
            .data_man
            .new_checkpoint(new_era_height, inner.best_epoch_number());
    }

    pub fn compute_outlier_bruteforce(
        inner: &ConsensusGraphInner, me: usize,
    ) -> BitSet {
        let parent = inner.arena[me].parent;
        if parent == NULL {
            // This is genesis, so the outlier should be empty
            return BitSet::new();
        }
        let mut last_in_main = inner.arena[parent].data.last_main_in_past;
        for referee in &inner.arena[me].referees {
            last_in_main =
                max(last_in_main, inner.arena[*referee].data.last_main_in_past);
        }
        let mut visited = BitSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(me);
        visited.add(me as u32);
        while let Some(index) = queue.pop_front() {
            let parent = inner.arena[index].parent;
            if parent != NULL
                && inner.arena[parent].data.epoch_number > last_in_main
                && !visited.contains(parent as u32)
            {
                visited.add(parent as u32);
                queue.push_back(parent);
            }
            for referee in &inner.arena[index].referees {
                if inner.arena[*referee].data.epoch_number > last_in_main
                    && !visited.contains(*referee as u32)
                {
                    visited.add(*referee as u32);
                    queue.push_back(*referee);
                }
            }
        }
        // Now we traverse all future of me, when adding new block, this is
        // empty
        queue.clear();
        queue.push_back(me);
        while let Some(index) = queue.pop_front() {
            for child in &inner.arena[index].children {
                if !visited.contains(*child as u32) {
                    visited.add(*child as u32);
                    queue.push_back(*child);
                }
            }
            for referrer in &inner.arena[index].referrers {
                if !visited.contains(*referrer as u32) {
                    visited.add(*referrer as u32);
                    queue.push_back(*referrer);
                }
            }
        }

        let mut outlier = BitSet::with_capacity(inner.arena.capacity() as u32);
        for (i, node) in inner.arena.iter() {
            if node.data.epoch_number > last_in_main
                && !visited.contains(i as u32)
                && (node.data.activated || node.data.inactive_dependency_cnt == NULL) /* We include only preactivated blocks */
                && node.era_block != NULL
            /* We exclude out-of-era blocks */
            {
                outlier.add(i as u32);
            }
        }
        outlier
    }

    pub fn compute_outlier_hashset_bruteforce(
        inner: &ConsensusGraphInner, me: usize,
    ) -> HashSet<usize> {
        let s = ConsensusNewBlockHandler::compute_outlier_bruteforce(inner, me);
        let mut ret = HashSet::new();
        for index in s.iter() {
            ret.insert(index as usize);
        }
        ret
    }

    /// Note that this function is not a pure computation function. It has the
    /// sideeffect of updating all existing outlier set in the outlier
    /// cache
    fn compute_and_update_outlier(
        inner: &mut ConsensusGraphInner, me: usize,
    ) -> (BitSet, BitSet) {
        let parent = inner.arena[me].parent;

        // If we do not have the outlier of its parent, we compute it with
        // brute force!
        let parent_outlier_opt = inner.outlier_cache.get(parent);
        let mut outlier;
        if parent_outlier_opt.is_none() {
            outlier =
                ConsensusNewBlockHandler::compute_outlier_bruteforce(inner, me);
        } else {
            // outlier = parent_outlier + parent_future - my_past
            // Compute future set of parent
            outlier = inner.compute_future_bitset(parent);
            outlier.remove(me as u32);

            for index in parent_outlier_opt.unwrap() {
                outlier.add(*index as u32);
            }
            let mut my_past = BitSet::new();
            let mut queue: VecDeque<usize> = VecDeque::new();
            queue.push_back(me);
            while let Some(index) = queue.pop_front() {
                if my_past.contains(index as u32) {
                    continue;
                }

                debug_assert!(index != parent);
                if index != me {
                    my_past.add(index as u32);
                }

                let idx_parent = inner.arena[index].parent;
                if idx_parent != NULL {
                    if outlier.contains(idx_parent as u32)
                        || inner.arena[idx_parent].era_block == NULL
                    {
                        queue.push_back(idx_parent);
                    }
                }

                for referee in &inner.arena[index].referees {
                    if outlier.contains(*referee as u32)
                        || inner.arena[*referee].era_block == NULL
                    {
                        queue.push_back(*referee);
                    }
                }
            }
            for index in my_past.drain() {
                outlier.remove(index);
            }

            // We only consider non-lagacy blocks when computing outlier.
            for index in outlier.clone().iter() {
                if inner.arena[index as usize].era_block == NULL {
                    outlier.remove(index);
                }
            }
        }

        inner.outlier_cache.update(me, &outlier);

        let mut outlier_barrier = BitSet::new();
        for index in outlier.clone().iter() {
            let parent = inner.arena[index as usize].parent as u32;
            if !outlier.contains(parent) {
                outlier_barrier.add(index);
            }
        }

        debug!(
            "Block {} outlier size {}",
            inner.arena[me].hash,
            outlier.len()
        );

        (outlier, outlier_barrier)
    }

    fn check_correct_parent_brutal(
        inner: &ConsensusGraphInner, me: usize, subtree_weight: &Vec<i128>,
        checking_candidate: Iter<usize>,
    ) -> bool {
        let mut valid = true;
        let parent = inner.arena[me].parent;
        let force_confirm = inner.arena[me].data.force_confirm;
        let force_confirm_height = inner.arena[force_confirm].height;

        // Check the main selection decision.
        for consensus_arena_index_in_epoch in checking_candidate {
            let lca = inner.lca(*consensus_arena_index_in_epoch, parent);
            assert!(lca != *consensus_arena_index_in_epoch);
            // If it is outside current era, we will skip!
            if lca == NULL || inner.arena[lca].height < force_confirm_height {
                continue;
            }
            if lca == parent {
                valid = false;
                break;
            }

            let fork = inner.ancestor_at(
                *consensus_arena_index_in_epoch,
                inner.arena[lca].height + 1,
            );
            let main = inner.ancestor_at(parent, inner.arena[lca].height + 1);

            let fork_subtree_weight = subtree_weight[fork];
            let main_subtree_weight = subtree_weight[main];

            if ConsensusGraphInner::is_heavier(
                (fork_subtree_weight, &inner.arena[fork].hash),
                (main_subtree_weight, &inner.arena[main].hash),
            ) {
                valid = false;
                break;
            }
        }

        valid
    }

    fn check_correct_parent(
        inner: &mut ConsensusGraphInner, me: usize, outlier_barrier: &BitSet,
        weight_tuple: Option<&Vec<i128>>,
    ) -> bool {
        let parent = inner.arena[me].parent;
        // FIXME: Because now we allow partial invalid blocks as parent, we need
        // to consider more for block candidates. This may cause a
        // performance issue and we should consider another optimized strategy.
        let mut candidate;
        let blockset =
            inner.exchange_or_compute_blockset_in_own_view_of_epoch(me, None);
        // Note that here we have to be conservative. If it is pending we have
        // to treat it as if it is partial invalid.
        let candidate_iter = if inner.arena[parent].data.partial_invalid
            || inner.arena[parent].data.pending
        {
            candidate = blockset.clone();
            let mut p = parent;
            while p != NULL && inner.arena[p].data.partial_invalid
                || inner.arena[p].data.pending
            {
                let blockset_p = inner
                    .exchange_or_compute_blockset_in_own_view_of_epoch(p, None);
                candidate.extend(blockset_p.iter());
                inner.exchange_or_compute_blockset_in_own_view_of_epoch(
                    p,
                    Some(blockset_p),
                );
                p = inner.arena[p].parent;
            }
            candidate.iter()
        } else {
            blockset.iter()
        };

        if let Some(subtree_weight) = weight_tuple {
            let res = ConsensusNewBlockHandler::check_correct_parent_brutal(
                inner,
                me,
                subtree_weight,
                candidate_iter,
            );
            // We have to put but the blockset here! Otherwise the
            // blockset_in_own_view_of_epoch will be corrupted.
            inner.exchange_or_compute_blockset_in_own_view_of_epoch(
                me,
                Some(blockset),
            );
            return res;
        }
        let mut valid = true;
        let force_confirm = inner.arena[me].data.force_confirm;
        let force_confirm_height = inner.arena[force_confirm].height;

        let mut weight_delta = HashMap::new();

        for index in outlier_barrier {
            let delta = inner.weight_tree.get(index as usize);
            weight_delta.insert(index as usize, delta);
        }

        // Remove weight contribution of outlier
        for (index, delta) in &weight_delta {
            inner.weight_tree.path_apply(*index, -delta);
        }

        // Check the main selection decision.
        for consensus_arena_index_in_epoch in candidate_iter {
            let lca = inner.lca(*consensus_arena_index_in_epoch, parent);
            assert!(lca != *consensus_arena_index_in_epoch);
            // If it is outside the era, we will skip!
            if lca == NULL || inner.arena[lca].height < force_confirm_height {
                continue;
            }
            if lca == parent {
                debug!("Block invalid (index = {}), referenced block {} index {} is in the subtree of parent block {} index {}!", me, inner.arena[*consensus_arena_index_in_epoch].hash, *consensus_arena_index_in_epoch, inner.arena[parent].hash, parent);
                valid = false;
                break;
            }

            let fork = inner.ancestor_at(
                *consensus_arena_index_in_epoch,
                inner.arena[lca].height + 1,
            );
            let main = inner.ancestor_at(parent, inner.arena[lca].height + 1);

            let fork_subtree_weight = inner.weight_tree.get(fork);
            let main_subtree_weight = inner.weight_tree.get(main);

            if ConsensusGraphInner::is_heavier(
                (fork_subtree_weight, &inner.arena[fork].hash),
                (main_subtree_weight, &inner.arena[main].hash),
            ) {
                debug!("Block invalid (index = {}), referenced block {} index {} fork is heavier than the parent block {} index {} fork! Ref fork block {} weight {}, parent fork block {} weight {}!",
                       me, inner.arena[*consensus_arena_index_in_epoch].hash, *consensus_arena_index_in_epoch, inner.arena[parent].hash, parent,
                       inner.arena[fork].hash, fork_subtree_weight, inner.arena[main].hash, main_subtree_weight);
                valid = false;
                break;
            } else {
                trace!("Pass one validity check, block index = {}. Referenced block {} index {} fork is not heavier than the parent block {} index {} fork. Ref fork block {} weight {}, parent fork block {} weight {}!",
                       me, inner.arena[*consensus_arena_index_in_epoch].hash, *consensus_arena_index_in_epoch, inner.arena[parent].hash, parent,
                       inner.arena[fork].hash, fork_subtree_weight, inner.arena[main].hash, main_subtree_weight);
            }
        }

        inner.exchange_or_compute_blockset_in_own_view_of_epoch(
            me,
            Some(blockset),
        );

        for (index, delta) in &weight_delta {
            inner.weight_tree.path_apply(*index, *delta);
        }

        valid
    }

    fn check_block_full_validity(
        &self, new: usize, inner: &mut ConsensusGraphInner, _adaptive: bool,
        outlier_barrier: &BitSet, weight_tuple: Option<&Vec<i128>>,
    ) -> bool {
        let parent = inner.arena[new].parent;
        let force_confirm = inner.arena[new].data.force_confirm;

        if inner.lca(parent, force_confirm) != force_confirm {
            warn!("Partially invalid due to picking incorrect parent (force confirmation {:?} violation). {:?}", force_confirm, inner.arena[new].hash);
            return false;
        }

        // Check whether the new block select the correct parent block
        if !ConsensusNewBlockHandler::check_correct_parent(
            inner,
            new,
            outlier_barrier,
            weight_tuple,
        ) {
            warn!(
                "Partially invalid due to picking incorrect parent. {:?}",
                inner.arena[new].hash
            );
            return false;
        }

        // Check whether difficulty is set correctly
        if inner.arena[new].difficulty
            != inner.expected_difficulty(&inner.arena[parent].hash)
        {
            warn!(
                "Partially invalid due to wrong difficulty. {:?}",
                inner.arena[new].hash
            );
            return false;
        }

        // Check adaptivity match. Note that in bench mode we do not check
        // the adaptive field correctness. We simply override its value
        // with the right one.
        
        return true;
    }

    #[inline]
    /// Subroutine called by on_new_block()
    fn update_lcts_initial(&self, inner: &mut ConsensusGraphInner, me: usize) {
        let parent = inner.arena[me].parent;

        inner.weight_tree.make_tree(me);
        inner.weight_tree.link(parent, me);

        inner.adaptive_tree.make_tree(me);
        inner.adaptive_tree.link(parent, me);
    }

    #[inline]
    /// Subroutine called by on_new_block()
    fn update_lcts_finalize(&self, inner: &mut ConsensusGraphInner, me: usize) {
        let parent = inner.arena[me].parent;
        let parent_tw = inner.weight_tree.get(parent);
        let parent_w = inner.block_weight(parent);
        inner.adaptive_tree.set(me, -parent_tw + parent_w);

        let weight = inner.block_weight(me);
        inner.weight_tree.path_apply(me, weight);
        inner.adaptive_tree.path_apply(me, 2 * weight);
        inner.adaptive_tree.caterpillar_apply(parent, -weight);
    }

    fn recycle_tx_in_block(
        &self, inner: &ConsensusGraphInner, block_hash: &H256,
    ) {
        info!("recycle_tx_in_block: block_hash={:?}", block_hash);
        if let Some(block) = inner
            .data_man
            .block_by_hash(block_hash, true /* update_cache */)
        {
            self.txpool.recycle_transactions(block.transactions.clone());
        } else {
            // This should only happen for blocks in the outlier of
            // checkpoints.
            warn!("recycle_tx_in_block: block {:?} not in db", block_hash);
        }
    }

    fn should_move_stable_height(
        &self, inner: &mut ConsensusGraphInner,
    ) -> u64 {
        if let Some(sync_state_starting_epoch) =
            self.conf.sync_state_starting_epoch
        {
            if inner.header_only
                && inner.cur_era_stable_height == sync_state_starting_epoch
            {
                // We want to use sync_state_starting_epoch as our stable
                // checkpoint when we enter
                // CatchUpCheckpointPhase, so we do not want to move forward our
                // stable checkpoint. Since we will enter
                // CatchUpCheckpointPhase the next time we check phase changes,
                // it's impossible for the delayed checkpoint making to cause
                // OOM.
                return inner.cur_era_stable_height;
            }
        }
        let new_stable_height =
            inner.cur_era_stable_height + inner.inner_conf.era_epoch_count;
        // We make sure there is an additional era before the best for moving it
        if new_stable_height + inner.inner_conf.era_epoch_count
            > inner.best_epoch_number()
        {
            return inner.cur_era_stable_height;
        }
        let new_stable_main_arena_index =
            inner.get_main_block_arena_index(new_stable_height);
        // Now we need to make sure that this new stable block is
        // force_confirmed in our current graph
        if inner.timer_chain_accumulative_lca.len() == 0 {
            return inner.cur_era_stable_height;
        }
        if let Some(last) = inner.timer_chain_accumulative_lca.last() {
            let lca = inner.lca(*last, new_stable_main_arena_index);
            if lca == new_stable_main_arena_index {
                return new_stable_height;
            }
        }
        return inner.cur_era_stable_height;
    }

    fn should_form_checkpoint_at(
        &self, inner: &mut ConsensusGraphInner,
    ) -> usize {
        let stable_main_block =
            inner.get_main_block_arena_index(inner.cur_era_stable_height);
        let mut new_genesis_height =
            inner.cur_era_genesis_height + inner.inner_conf.era_epoch_count;

        // FIXME: Here is a chicken and egg problem. In our full node sync
        // FIXME: logic, we first run consensus on headers to determine
        // FIXME: the checkpoint location. And then run the full blocks.
        // FIXME: However, when we do not have the body, we cannot faithfully
        // FIXME: check this condition. The consequence is that if
        // FIXME: attacker managed to generate a lot blame blocks. New full
        // FIXME: nodes will not correctly determine the safe checkpoint
        // FIXME: location to start the sync. Causing potential panic
        // FIXME: when computing `state_valid` and `blame_info`.
        if !inner.header_only {
            // Stable block must have a blame vector that does not stretch
            // beyond the new genesis
            if !inner.arena[stable_main_block].data.state_valid.unwrap() {
                if inner.arena[stable_main_block]
                    .data
                    .blame_info
                    .unwrap()
                    .blame as u64
                    + new_genesis_height
                    + DEFERRED_STATE_EPOCH_COUNT
                    >= inner.cur_era_stable_height
                {
                    return inner.cur_era_genesis_block_arena_index;
                }
            }
        }

        // We cannot move beyond the stable block/height
        'out: while new_genesis_height < inner.cur_era_stable_height {
            let new_genesis_block_arena_index =
                inner.get_main_block_arena_index(new_genesis_height);
            assert!(inner.arena[stable_main_block].data.force_confirm != NULL);
            if inner.lca(
                new_genesis_block_arena_index,
                inner.arena[stable_main_block].data.force_confirm,
            ) != new_genesis_block_arena_index
            {
                // All following era genesis candidates are on the same fork,
                // so they are not force_confirmed by stable now.
                return inner.cur_era_genesis_block_arena_index;
            }

            // Because the timer chain is unlikely to reorganize at this point.
            // We will just skip this height if we found timer block
            // in its outlier before.
            if inner
                .has_timer_block_in_outlier_cache
                .contains(&new_genesis_block_arena_index)
            {
                new_genesis_height += inner.inner_conf.era_epoch_count;
                continue 'out;
            }

            // Now we need to make sure that no timer chain block is in the
            // outlier of the new genesis. This is required for our
            // checkpoint algorithm.
            let mut visited = BitSet::new();
            let mut queue = VecDeque::new();
            queue.push_back(new_genesis_block_arena_index);
            visited.add(new_genesis_block_arena_index as u32);
            while let Some(x) = queue.pop_front() {
                for child in &inner.arena[x].children {
                    if !visited.contains(*child as u32) {
                        visited.add(*child as u32);
                        queue.push_back(*child);
                    }
                }
                for referrer in &inner.arena[x].referrers {
                    if !visited.contains(*referrer as u32) {
                        visited.add(*referrer as u32);
                        queue.push_back(*referrer);
                    }
                }
            }
            let start_timer_chain_height = inner.arena
                [new_genesis_block_arena_index]
                .data
                .ledger_view_timer_chain_height;
            let start_timer_chain_index = (start_timer_chain_height
                - inner.cur_era_genesis_timer_chain_height)
                as usize;
            for i in start_timer_chain_index..inner.timer_chain.len() {
                if !visited.contains(inner.timer_chain[i] as u32) {
                    inner
                        .has_timer_block_in_outlier_cache
                        .insert(new_genesis_block_arena_index);
                    // This era genesis candidate has a timer chain block in its
                    // outlier, so we move to check the next one.
                    new_genesis_height += inner.inner_conf.era_epoch_count;
                    continue 'out;
                }
            }
            return new_genesis_block_arena_index;
        }
        // We cannot make a new checkpoint.
        inner.cur_era_genesis_block_arena_index
    }

    fn persist_terminals(&self, inner: &ConsensusGraphInner) {
        let mut terminals = Vec::with_capacity(inner.terminal_hashes.len());
        for h in &inner.terminal_hashes {
            terminals.push(h.clone());
        }
        self.data_man.insert_terminals_to_db(terminals);
    }

    fn try_clear_blockset_in_own_view_of_epoch(
        inner: &mut ConsensusGraphInner, me: usize,
    ) {
        if inner.arena[me].data.blockset_in_own_view_of_epoch.len() as u64
            > BLOCKSET_IN_OWN_VIEW_OF_EPOCH_CAP
        {
            inner.arena[me].data.blockset_in_own_view_of_epoch =
                Default::default();
            inner.arena[me].data.skipped_epoch_blocks = Default::default();
            inner.arena[me].data.blockset_cleared = true;
        }
    }

    // This function computes the timer chain in the view of the new block.
    // The first returned value is the fork height of the timer chain.
    // The second is a map that overwrites timer_chain_height values after the
    // fork height.
    fn compute_timer_chain_tuple(
        inner: &ConsensusGraphInner, me: usize, outlier: &BitSet,
    ) -> (u64, HashMap<usize, u64>, Vec<usize>, Vec<usize>) {
        inner.compute_timer_chain_tuple(
            inner.arena[me].parent,
            &inner.arena[me].referees,
            Some(outlier),
        )
    }

    fn compute_invalid_block_start_timer(
        &self, inner: &ConsensusGraphInner, me: usize,
    ) -> u64 {
        let last_index =
            inner.arena[me].data.past_view_last_timer_block_arena_index;
        if last_index == NULL {
            inner.inner_conf.timer_chain_beta
        } else {
            inner.arena[last_index].data.ledger_view_timer_chain_height
                + inner.inner_conf.timer_chain_beta
                + if inner.get_timer_chain_index(last_index) != NULL {
                    1
                } else {
                    0
                }
        }
    }

    fn preactivate_block(
        &self, inner: &mut ConsensusGraphInner, me: usize,
    ) -> BlockStatus {
        debug!(
            "Start to preactivate block {} index = {}",
            inner.arena[me].hash, me
        );
        let parent = inner.arena[me].parent;
        let mut pending = {
            if let Some(f) = inner.initial_stable_future.as_mut() {
                let mut in_future = false;
                if inner.arena[me].hash == inner.cur_era_stable_block_hash {
                    in_future = true;
                }
                if parent != NULL && f.contains(parent as u32) {
                    in_future = true;
                }
                if !in_future {
                    for referee in &inner.arena[me].referees {
                        if f.contains(*referee as u32) {
                            in_future = true;
                            break;
                        }
                    }
                }
                if in_future {
                    f.add(me as u32);
                }
                !in_future
            } else {
                let mut last_main_in_past = if parent != NULL {
                    inner.arena[parent].data.last_main_in_past
                } else {
                    inner.cur_era_genesis_height
                };
                for referee in &inner.arena[me].referees {
                    last_main_in_past = max(
                        last_main_in_past,
                        inner.arena[*referee].data.last_main_in_past,
                    );
                }
                last_main_in_past < inner.cur_era_stable_height
            }
        };

        // Because the following computation relies on all previous blocks being
        // active, We have to delay it till now
        let (timer_longest_difficulty, last_timer_block_arena_index) = inner
            .compute_timer_chain_past_view_info(
                parent,
                &inner.arena[me].referees,
            );

        inner.arena[me].data.past_view_timer_longest_difficulty =
            timer_longest_difficulty;
        inner.arena[me].data.past_view_last_timer_block_arena_index =
            last_timer_block_arena_index;

        inner.arena[me].data.force_confirm =
            inner.cur_era_genesis_block_arena_index;

        let fully_valid;

        // Note that this function also updates the outlier for other nodes, so
        // we have to call it even for pending blocks!
        let (outlier, outlier_barrier) =
            ConsensusNewBlockHandler::compute_and_update_outlier(inner, me);

        if !pending {
            let timer_chain_tuple =
                ConsensusNewBlockHandler::compute_timer_chain_tuple(
                    inner, me, &outlier,
                );

            inner.arena[me].data.force_confirm =
                inner.compute_block_force_confirm(&timer_chain_tuple);
            debug!(
                "Force confirm block index {} in the past view of block index={}",
                inner.arena[me].data.force_confirm, me
            );

            let weight_tuple = if outlier_barrier.len() >= OUTLIER_BARRIER_CAP {
                Some(inner.compute_subtree_weights(me, &outlier_barrier))
            } else {
                None
            };

            let adaptive = inner.adaptive_weight(
                me,
                &outlier_barrier,
                weight_tuple.as_ref(),
                &timer_chain_tuple,
            );

            fully_valid = self.check_block_full_validity(
                me,
                inner,
                adaptive,
                &outlier_barrier,
                weight_tuple.as_ref(),
            );

            if fully_valid {
                inner.arena[me].adaptive = adaptive;
            }
        } else {
            let block_status_in_db = self
                .data_man
                .local_block_info_by_hash(&inner.arena[me].hash)
                .map(|info| info.get_status())
                .unwrap_or(BlockStatus::Pending);
            fully_valid = block_status_in_db != BlockStatus::PartialInvalid;
            pending = block_status_in_db == BlockStatus::Pending;
            debug!(
                "Fetch the block validity status {} from the local data base",
                fully_valid
            );
        }

        debug!(
            "Finish preactivation block {} index = {}",
            inner.arena[me].hash, me
        );
        let block_status = if pending {
            BlockStatus::Pending
        } else if fully_valid {
            BlockStatus::Valid
        } else {
            BlockStatus::PartialInvalid
        };
        self.persist_block_info(inner, me, block_status);

        block_status
    }

    fn activate_block(
        &self, inner: &mut ConsensusGraphInner, me: usize,
        meter: &ConfirmationMeter, queue: &mut VecDeque<usize>,
    ) {
        inner.arena[me].data.activated = true;
        self.statistics.inc_consensus_graph_activated_block_count();
        let mut succ_list = inner.arena[me].children.clone();
        succ_list.extend(inner.arena[me].referrers.iter());
        for succ in &succ_list {
            assert!(inner.arena[*succ].data.inactive_dependency_cnt > 0);
            inner.arena[*succ].data.inactive_dependency_cnt -= 1;
            if inner.arena[*succ].data.inactive_dependency_cnt == 0 {
                queue.push_back(*succ);
            }
        }
        // The above is the only thing we need to do for out-of-era blocks
        // so for these blocks, we quit here.
        if inner.arena[me].era_block == NULL {
            debug!(
                "Updated active counters for out-of-era block in ConsensusGraph: index = {:?} hash={:?}",
                me, inner.arena[me].hash,
            );
            return;
        } else {
            debug!(
                "Start activating block in ConsensusGraph: index = {:?} hash={:?}",
                me, inner.arena[me].hash,
            );
        }

        let parent = inner.arena[me].parent;
        // Update terminal hashes for mining
        if parent != NULL {
            inner.terminal_hashes.remove(&inner.arena[parent].hash);
        }
        inner.terminal_hashes.insert(inner.arena[me].hash.clone());
        for referee in &inner.arena[me].referees {
            inner.terminal_hashes.remove(&inner.arena[*referee].hash);
        }

        self.update_lcts_finalize(inner, me);
        let my_weight = inner.block_weight(me);
        let mut extend_main = false;
        let mut main_changed = false;
        // ``fork_at`` stores the first main chain height that we need to
        // update (because of the new inserted block). If the new block
        // extends the main chain, ``fork_at`` will equal to the new main
        // chain height (end of the main chain).
        let mut fork_at;
        let old_main_chain_len = inner.main_chain.len();

        // Now we are going to maintain the timer chain.
        let diff = inner.arena[me].data.past_view_timer_longest_difficulty
            + inner.get_timer_difficulty(me);
        if inner.arena[me].is_timer
            && !inner.arena[me].data.partial_invalid
            && ConsensusGraphInner::is_heavier(
                (diff, &inner.arena[me].hash),
                (
                    inner.best_timer_chain_difficulty,
                    &inner.best_timer_chain_hash,
                ),
            )
        {
            inner.best_timer_chain_difficulty = diff;
            inner.best_timer_chain_hash = inner.arena[me].hash.clone();
            inner.update_timer_chain(me);
            // Now we go over every element in the ``invalid_block_queue``
            // because their timer may change.
            
            let mut timer_chain_height =
                inner.arena[parent].data.ledger_view_timer_chain_height;
            if inner.get_timer_chain_index(parent) != NULL {
                timer_chain_height += 1;
            }
            for referee in &inner.arena[me].referees {
                let timer_bit = if inner.get_timer_chain_index(*referee) != NULL
                {
                    1
                } else {
                    0
                };
                if inner.arena[*referee].data.ledger_view_timer_chain_height
                    + timer_bit
                    > timer_chain_height
                {
                    timer_chain_height = inner.arena[*referee]
                        .data
                        .ledger_view_timer_chain_height
                        + timer_bit;
                }
            }
            inner.arena[me].data.ledger_view_timer_chain_height =
                timer_chain_height;
        }

        meter.aggregate_total_weight_in_past(my_weight);
        let force_confirm = inner.compute_global_force_confirm();
        let force_height = inner.arena[force_confirm].height;
        let last = inner.main_chain.last().cloned().unwrap();
        let force_lca = inner.lca(force_confirm, last);

        if force_lca == force_confirm && inner.arena[me].parent == last {
            inner.main_chain.push(me);
            inner.set_epoch_number_in_epoch(
                me,
                inner.main_index_to_height(inner.main_chain.len()) - 1,
            );
            inner.main_chain_metadata.push(Default::default());
            extend_main = true;
            main_changed = true;
            fork_at = inner.main_index_to_height(old_main_chain_len)
        } else {
            let lca = inner.lca(last, me);
            let new;
            if force_confirm != force_lca {
                debug!(
                    "main chain switch to force_confirm={} force_height={}",
                    force_confirm, force_height
                );
                fork_at = inner.arena[force_lca].height + 1;
                new = inner.ancestor_at(force_confirm, fork_at);
                main_changed = true;
            } else {
                fork_at = inner.arena[lca].height + 1;
                let prev = inner.get_main_block_arena_index(fork_at);
                let prev_weight = inner.weight_tree.get(prev);
                new = inner.ancestor_at(me, fork_at);
                let new_weight = inner.weight_tree.get(new);

                // Note that for properly set consensus parameters, fork_at will
                // always after the force_height (i.e., the
                // force confirmation is always stable).
                // But during testing, we may want to stress the consensus.
                // Therefore we add this condition fork_at >
                // force_height to maintain consistency.
                if fork_at > force_height
                    && ConsensusGraphInner::is_heavier(
                        (new_weight, &inner.arena[new].hash),
                        (prev_weight, &inner.arena[prev].hash),
                    )
                {
                    main_changed = true;
                } else {
                    // The previous subtree is still heavier, nothing is
                    // updated
                    debug!("Old main chain is heavier, main chain unchanged");
                    fork_at = inner.main_index_to_height(old_main_chain_len);
                }
            }
            if main_changed {
                // The new subtree is heavier, update main chain
                let fork_main_index = inner.height_to_main_index(fork_at);
                assert!(fork_main_index < inner.main_chain.len());
                for discarded_idx in inner.main_chain.split_off(fork_main_index)
                {
                    // Reset the epoch_number of the discarded fork
                    inner.reset_epoch_number_in_epoch(discarded_idx);
                    ConsensusNewBlockHandler::try_clear_blockset_in_own_view_of_epoch(inner,
                    discarded_idx);
                }
                let mut u = new;
                loop {
                    inner.compute_blockset_in_own_view_of_epoch(u);
                    inner.main_chain.push(u);
                    inner.set_epoch_number_in_epoch(
                        u,
                        inner.main_index_to_height(inner.main_chain.len()) - 1,
                    );
                    if inner.arena[u].height >= force_height {
                        let mut heaviest = NULL;
                        let mut heaviest_weight = 0;
                        for index in &inner.arena[u].children {
                            if !inner.arena[*index].data.activated {
                                continue;
                            }
                            let weight = inner.weight_tree.get(*index);
                            if heaviest == NULL
                                || ConsensusGraphInner::is_heavier(
                                    (weight, &inner.arena[*index].hash),
                                    (
                                        heaviest_weight,
                                        &inner.arena[heaviest].hash,
                                    ),
                                )
                            {
                                heaviest = *index;
                                heaviest_weight = weight;
                            }
                        }
                        if heaviest == NULL {
                            break;
                        }
                        u = heaviest;
                    } else {
                        u = inner.ancestor_at(
                            force_confirm,
                            inner.arena[u].height + 1,
                        );
                    }
                }
            }
        };
        debug!(
            "Forked at height {}, fork parent block {}",
            fork_at,
            &inner.arena[inner.get_main_block_arena_index(fork_at - 1)].hash,
        );

        // Now compute last_main_in_block and update main_metadata.
        // Note that we need to do this for partially invalid blocks to
        // propagate information!
        if !extend_main {
            let update_at = fork_at - 1;
            let mut last_main_to_update = HashSet::new();
            last_main_to_update.insert(me);
            if main_changed {
                inner.best_terminals_reorg_height =
                    min(inner.best_terminals_reorg_height, update_at);
                let update_main_index = inner.height_to_main_index(update_at);
                for main_index in update_main_index..old_main_chain_len {
                    for x in &inner.main_chain_metadata[main_index]
                        .last_main_in_past_blocks
                    {
                        last_main_to_update.insert(*x);
                    }
                }
                inner.recompute_metadata(fork_at, last_main_to_update);
            } else {
                // main chain not extend and not change
                ConsensusNewBlockHandler::try_clear_blockset_in_own_view_of_epoch(inner, me);
                inner.recompute_metadata(
                    inner.get_main_height(),
                    last_main_to_update,
                );
            }
        } else {
            let height = inner.arena[me].height;
            inner.arena[me].data.last_main_in_past = height;
            let main_index = inner.height_to_main_index(height);
            inner.main_chain_metadata[main_index]
                .last_main_in_past_blocks
                .insert(me);
            let blockset = inner
                .exchange_or_compute_blockset_in_own_view_of_epoch(me, None);
            inner.main_chain_metadata[main_index].past_weight =
                inner.main_chain_metadata[main_index - 1].past_weight
                    + inner.block_weight(me);
            inner.exchange_or_compute_blockset_in_own_view_of_epoch(
                me,
                Some(blockset),
            );
        }

        // Only process blocks in the subtree of stable
        if inner.arena[me].height <= inner.cur_era_stable_height
            || (inner.arena[me].height > inner.cur_era_stable_height
                && inner.arena[me]
                    .hash
                    != inner.cur_era_stable_block_hash)
        {
            self.persist_terminals(inner);
            if main_changed {
                // If we switch to a chain without stable block,
                // we should avoid execute unavailable states.
                // TODO It is handled by processing
                // `state_availability_boundary` at the end,
                // we can probably refactor to move that part of code before
                // this skip and remove this special case.
                self.data_man
                    .state_availability_boundary
                    .write()
                    .optimistic_executed_height = None;
            }
            debug!(
                "Finish activating block in ConsensusGraph: index={:?} hash={:?},\
                 block is not in the subtree of stable",
                me, inner.arena[me].hash
            );
            return;
        }
        // Note that only main chain height after the capped_fork_at needs to
        // update execution state.
        let capped_fork_at = max(inner.cur_era_stable_height + 1, fork_at);

        inner.adjust_difficulty(*inner.main_chain.last().expect("not empty"));
        if me % CONFIRMATION_METER_UPDATE_FREQUENCY == 0 || main_changed {
            meter.update_confirmation_risks(inner);
        }

        if main_changed {
            if inner.main_chain.len() > EPOCH_SET_PERSISTENCE_DELAY as usize {
                let capped_fork_at_main_index =
                    inner.height_to_main_index(capped_fork_at);
                // Starting from old_len ensures that all epochs within
                // [old_len - delay, new_len - delay) will be inserted to db, so
                // no epochs will be skipped. Starting from
                // fork_at ensures that any epoch set change will be
                // overwritten.
                let start_main_index = if old_main_chain_len
                    >= EPOCH_SET_PERSISTENCE_DELAY as usize
                {
                    min(
                        capped_fork_at_main_index,
                        old_main_chain_len
                            - EPOCH_SET_PERSISTENCE_DELAY as usize,
                    )
                } else {
                    capped_fork_at_main_index
                };
                let to_persist_main_index = inner.main_chain.len()
                    - EPOCH_SET_PERSISTENCE_DELAY as usize;
                for main_index in start_main_index..to_persist_main_index {
                    inner.persist_epoch_set_hashes(main_index);
                }
            }
        }

        // Note that after the checkpoint (if happens), the old_main_chain_len
        // value will become obsolete
        let old_main_chain_height =
            inner.main_index_to_height(old_main_chain_len);

        if inner.best_epoch_number() > inner.cur_era_stable_height
            && inner.arena
                [inner.get_main_block_arena_index(inner.cur_era_stable_height)]
            .hash
                == inner.cur_era_stable_block_hash
        {
            let new_stable_height = self.should_move_stable_height(inner);
            if inner.cur_era_stable_height != new_stable_height {
                inner.cur_era_stable_height = new_stable_height;
                let stable_arena_index =
                    inner.get_main_block_arena_index(new_stable_height);

                // Ensure all blocks on the main chain before
                // the new stable block to have state_valid computed
                if !inner.header_only {
                    // FIXME: this asserion doesn't hold any more
                    // assert!(
                    //     new_stable_height
                    //         >= inner
                    //             .data_man
                    //             .state_availability_boundary
                    //             .read()
                    //             .lower_bound
                    // );
                    // If new_era_genesis should have available state,
                    // make sure state execution is finished before setting
                    // lower_bound
                    // to the new_checkpoint_era_genesis.
                    self.executor
                        .wait_for_result(inner.arena[stable_arena_index].hash)
                        .expect(
                            "Execution state of the main chain is corrupted!",
                        );
                    inner
                        .compute_state_valid_and_blame_info(
                            stable_arena_index,
                            &self.executor,
                        )
                        .expect(
                            "New stable node should have available state_valid",
                        );
                }

                let genesis_hash =
                    &inner.arena[inner.cur_era_genesis_block_arena_index].hash;
                let stable_hash = &inner.arena[stable_arena_index].hash;
                inner.cur_era_stable_block_hash = stable_hash.clone();
                inner.data_man.set_cur_consensus_era_genesis_hash(
                    genesis_hash,
                    stable_hash,
                );
                inner.initial_stable_future = None;
                debug!(
                    "Move era stable genesis to height={} hash={:?}",
                    new_stable_height, stable_hash
                );
            }
        }

        // We are only going to check the checkpoint movement after the stable
        // is on the main chain (will not always be true during the recovery).
        // The code inside assumes this assumption.
        if inner.cur_era_stable_height < inner.best_epoch_number()
            && inner.arena
                [inner.get_main_block_arena_index(inner.cur_era_stable_height)]
            .hash
                == inner.cur_era_stable_block_hash
        {
            let new_checkpoint_era_genesis =
                self.should_form_checkpoint_at(inner);
            if new_checkpoint_era_genesis
                != inner.cur_era_genesis_block_arena_index
            {
                info!(
                    "Working on new checkpoint, old checkpoint block {} height {}",
                    &inner.arena[inner.cur_era_genesis_block_arena_index].hash,
                    inner.cur_era_genesis_height
                );

                ConsensusNewBlockHandler::make_checkpoint_at(
                    inner,
                    new_checkpoint_era_genesis,
                );
                let stable_era_genesis_arena_index =
                    inner.ancestor_at(me, inner.cur_era_stable_height);
                meter.reset_for_checkpoint(
                    inner.weight_tree.get(stable_era_genesis_arena_index),
                    inner.cur_era_stable_height,
                );
                meter.update_confirmation_risks(inner);
                info!(
                    "New checkpoint formed at block {} stable block {} height {}",
                    &inner.arena[inner.cur_era_genesis_block_arena_index].hash,
                    &inner.arena[stable_era_genesis_arena_index].hash,
                    inner.cur_era_genesis_height
                );
            }
        }

        // send updated main chain to pubsub
        let from = capped_fork_at;
        let to = inner.main_index_to_height(inner.main_chain.len());

        for epoch_number in from..to {
            let arena_index = inner.get_main_block_arena_index(epoch_number);
            let epoch_hashes = inner.get_epoch_block_hashes(arena_index);

            // send epoch to pub-sub layer
            self.epochs_sender.send((epoch_number, epoch_hashes));

            // send epoch to blame verifier
            if let NodeType::Light = self.node_type {
                // ConsensusNewBlockHandler is single-threaded,
                // lock should always succeed.
                self.blame_verifier.lock().process(inner, epoch_number);
            }
        }

        // If we are inserting header only, we will skip execution and
        // tx_pool-related operations
        if !inner.header_only {
            // FIXME: Now we have to pass a conservative stable_height here.
            // FIXME: Because the storage layer does not handle the case when
            // FIXME: this confirmed point being reverted. We have to be extra
            // FIXME: conservatively but this will cost storage space.
            // FIXME: Eventually, we should implement the logic to recover from
            // FIXME: the database if such a rare reversion case happens.
            //
            // FIXME: we need a function to compute the deferred epoch
            // FIXME: number. the current codebase may not be
            // FIXME: consistent at all places.
            let mut confirmed_height = meter.get_confirmed_epoch_num();
            if confirmed_height < DEFERRED_STATE_EPOCH_COUNT {
                confirmed_height = 0;
            } else {
                confirmed_height -= DEFERRED_STATE_EPOCH_COUNT;
            }
            // We can not assume that confirmed epoch are already executed,
            // but we can assume that the deferred block are executed.
            self.data_man
                .storage_manager
                .get_storage_manager()
                .maintain_state_confirmed(
                    inner,
                    inner.cur_era_stable_height,
                    inner.inner_conf.era_epoch_count,
                    confirmed_height,
                    &self.data_man.state_availability_boundary,
                )
                // FIXME: propogate error.
                .expect(&concat!(file!(), ":", line!(), ":", column!()));
            self.set_block_tx_packed(inner, me);
            self.delayed_tx_recycle_in_skipped_blocks(inner, capped_fork_at);

            let to_state_pos = if inner
                .main_index_to_height(inner.main_chain.len())
                < DEFERRED_STATE_EPOCH_COUNT
            {
                0
            } else {
                inner.main_index_to_height(inner.main_chain.len())
                    - DEFERRED_STATE_EPOCH_COUNT
                    + 1
            };
            let mut state_at = capped_fork_at;
            if capped_fork_at + DEFERRED_STATE_EPOCH_COUNT
                > old_main_chain_height
            {
                if old_main_chain_height > DEFERRED_STATE_EPOCH_COUNT {
                    state_at =
                        old_main_chain_height - DEFERRED_STATE_EPOCH_COUNT + 1;
                } else {
                    state_at = 1;
                }
            }
            {
                let mut state_availability_boundary =
                    inner.data_man.state_availability_boundary.write();
                if main_changed {
                    assert!(
                        capped_fork_at > state_availability_boundary.lower_bound,
                        "forked_at {} should > boundary_lower_bound, boundary {:?}",
                        capped_fork_at,
                        state_availability_boundary
                    );
                    if extend_main {
                        state_availability_boundary
                            .main_chain
                            .push(inner.arena[me].hash);
                    } else {
                        let split_off_index = capped_fork_at
                            - state_availability_boundary.lower_bound;
                        state_availability_boundary
                            .main_chain
                            .truncate(split_off_index as usize);
                        for i in inner.height_to_main_index(capped_fork_at)
                            ..inner.main_chain.len()
                        {
                            state_availability_boundary
                                .main_chain
                                .push(inner.arena[inner.main_chain[i]].hash);
                        }
                        if state_availability_boundary.upper_bound
                            >= capped_fork_at
                        {
                            state_availability_boundary.upper_bound =
                                capped_fork_at - 1;
                        }
                    }
                    state_availability_boundary.optimistic_executed_height =
                        if to_state_pos
                            > state_availability_boundary.lower_bound
                        {
                            Some(to_state_pos)
                        } else {
                            None
                        };
                }
                // For full node, we don't execute blocks before available
                // states. This skip should only happen in
                // `SyncBlockPhase` for full nodes
                if state_at < state_availability_boundary.lower_bound + 1 {
                    state_at = state_availability_boundary.lower_bound + 1;
                }
            }

            // Apply transactions in the determined total order
            while state_at < to_state_pos {
                let epoch_arena_index =
                    inner.get_main_block_arena_index(state_at);
                let reward_execution_info = self
                    .executor
                    .get_reward_execution_info(inner, epoch_arena_index);
                self.executor.enqueue_epoch(EpochExecutionTask::new(
                    epoch_arena_index,
                    inner,
                    reward_execution_info,
                    true,  /* on_local_main */
                    false, /* force_recompute */
                ));

                state_at += 1;
            }
        }

        self.persist_terminals(inner);
        debug!(
            "Finish activating block in ConsensusGraph: index={:?} hash={:?} cur_era_stable_height={} cur_era_genesis_height={}",
            me, inner.arena[me].hash, inner.cur_era_stable_height, inner.cur_era_genesis_height
        );
    }

    /// The top level function invoked by ConsensusGraph to insert a new block.
    pub fn on_new_block(
        &self, inner: &mut ConsensusGraphInner, meter: &ConfirmationMeter,
        hash: &H256,
    ) {
        let block_header = self
            .data_man
            .block_header_by_hash(hash)
            .expect("header exist for consensus");
        debug!(
            "insert new block into consensus: header_only={:?} block={:?}",
            inner.header_only, &block_header
        );
        let parent_hash = block_header.parent_hash();
        let parent_index = inner.hash_to_arena_indices.get(&parent_hash);
        let me = if parent_index.is_none()
            || inner.arena[*parent_index.unwrap()].era_block == NULL
        {
            // current block is outside of the current era.
            debug!(
                "parent={:?} not in consensus graph or not in the genesis subtree, inserted as an out-era block stub",
                parent_hash
            );
            let block_status_in_db = self
                .data_man
                .local_block_info_by_hash(hash)
                .map(|info| info.get_status())
                .unwrap_or(BlockStatus::Pending);
            let (sn, me) = inner.insert_out_era_block(
                &block_header,
                block_status_in_db == BlockStatus::PartialInvalid,
            );
            let block_info = LocalBlockInfo::new(
                block_status_in_db,
                sn,
                self.data_man.get_instance_id(),
            );
            self.data_man.insert_local_block_info(hash, block_info);
            // If me is NULL, it means that this block does not have any stub,
            // so we can safely ignore it in the consensus besides
            // update its sequence number in the data manager.
            if me == NULL {
                // Block body in the outlier of a checkpoint is not needed for
                // full nodes, but they are still needed to sync
                // an archive node in the current implementation (to make their
                // child blocks `graph_ready`), so we still keep
                // them for now.

                // self.data_man
                //     .remove_block_body(hash, true /* remove_db */);
                return;
            }
            me
        } else {
            let (me, indices_len) = inner.insert(&block_header);
            self.statistics
                .set_consensus_graph_inserted_block_count(indices_len);
            self.update_lcts_initial(inner, me);
            me
        };

        if inner.arena[me].data.inactive_dependency_cnt == 0 {
            let mut queue: VecDeque<usize> = VecDeque::new();
            queue.push_back(me);
            while let Some(me) = queue.pop_front() {
                // For out-of-era blocks, we just fetch the results from the
                // already filled field. We do not run
                // preactivate_block() on them.
                let block_status = if inner.arena[me].era_block != NULL {
                    self.preactivate_block(inner, me)
                } else {
                    if inner.arena[me].data.partial_invalid {
                        BlockStatus::PartialInvalid
                    } else {
                        BlockStatus::Pending
                    }
                };

                if block_status == BlockStatus::PartialInvalid {
                    inner.arena[me].data.partial_invalid = true;
                    let timer =
                        self.compute_invalid_block_start_timer(inner, me);
                    // We are not going to delay partial invalid blocks in the
                    // bench mode
                    inner.arena[me].data.inactive_dependency_cnt = NULL;
                    debug!(
                        "Block {} (hash = {}) is partially invalid, all of its future will be non-active till timer height {}",
                        me, inner.arena[me].hash, timer
                    );
                } else {
                    if block_status == BlockStatus::Pending {
                        inner.arena[me].data.pending = true;
                        debug!(
                            "Block {} (hash = {}) is pending but processed",
                            me, inner.arena[me].hash
                        );
                    } else {
                        debug!(
                            "Block {} (hash = {}) is fully valid",
                            me, inner.arena[me].hash
                        );
                    }
                    self.activate_block(inner, me, meter, &mut queue);
                }
                // Now we are going to check all invalid blocks in the delay
                // queue Activate them if the timer is up
                let timer = if let Some(x) = inner.timer_chain.last() {
                    inner.arena[*x].data.ledger_view_timer_chain_height + 1
                } else {
                    inner.cur_era_genesis_timer_chain_height
                };
                loop {
                    if let Some((t, _)) = inner.invalid_block_queue.peek() {
                        if timer < (-*t) as u64 {
                            break;
                        }
                    } else {
                        break;
                    }
                    let (_, x) = inner.invalid_block_queue.pop().unwrap();
                    assert!(
                        inner.arena[x].data.inactive_dependency_cnt == NULL
                    );
                    inner.arena[x].data.inactive_dependency_cnt = 0;
                    self.activate_block(inner, x, meter, &mut queue);
                }
            }
        } else {
            debug!(
                "Block {} (hash = {}) is non-active with active counter {}",
                me,
                inner.arena[me].hash,
                inner.arena[me].data.inactive_dependency_cnt
            );
        }
    }

    fn persist_block_info(
        &self, inner: &mut ConsensusGraphInner, me: usize,
        block_status: BlockStatus,
    ) {
        let block_info = LocalBlockInfo::new(
            block_status,
            inner.arena[me].data.sequence_number,
            self.data_man.get_instance_id(),
        );
        self.data_man
            .insert_local_block_info(&inner.arena[me].hash, block_info);
        let era_block = inner.arena[me].era_block();
        let era_block_hash = if era_block != NULL {
            inner.arena[era_block].hash
        } else {
            Default::default()
        };
        if inner.inner_conf.enable_state_expose {
            STATE_EXPOSER.consensus_graph.lock().block_state_vec.push(
                ConsensusGraphBlockState {
                    block_hash: inner.arena[me].hash,
                    best_block_hash: inner.best_block_hash(),
                    block_status: block_info.get_status(),
                    era_block_hash,
                    adaptive: inner.arena[me].adaptive(),
                },
            )
        }
    }

    /// construct_main_state() rebuild main chain state info from db
    /// avoiding intermediate redundant computation triggered by
    /// on_new_block().
    /// It also recovers receipts_root and logs_bloom_hash in main chain.
    /// This function is only invoked from recover_graph_from_db with
    /// header_only being false.
    pub fn construct_main_state(
        &self, inner: &mut ConsensusGraphInner, meter: &ConfirmationMeter,
    ) {
        // FIXME: this line doesn't exactly match its purpose.
        // FIXME: Is it the checkpoint or synced snapshot or could it be
        // anything else?
        let state_boundary_height =
            self.data_man.state_availability_boundary.read().lower_bound;
        let start_main_index =
            (state_boundary_height - inner.cur_era_genesis_height) as usize;
        debug!(
            "construct_main_state: start={}, main_chain.len()={}, state_boundary_height={}",
            start_main_index,
            inner.main_chain.len(),
            state_boundary_height
        );
        if start_main_index >= inner.main_chain.len() {
            // The main chain of recovered blocks is before state lower_bound,
            // so we do not need to construct any main state.
            return;
        }
        let start_hash = inner.arena[inner.main_chain[start_main_index]].hash;
        // Here, we should ensure the epoch_execution_commitment for stable hash
        // must be loaded into memory. Since, in some rare cases, the number of
        // blocks between stable and best_epoch is less than
        // DEFERRED_STATE_EPOCH_COUNT, the for loop below will not load
        // epoch_execution_commitment for stable hash.
        if start_hash != inner.data_man.true_genesis.hash()
            && self
                .data_man
                .get_epoch_execution_commitment(&start_hash)
                .is_none()
        {
            self.data_man.load_epoch_execution_commitment_from_db(&start_hash)
                .expect("epoch_execution_commitment for stable hash must exist in disk");
        }
        {
            let mut state_availability_boundary =
                self.data_man.state_availability_boundary.write();
            assert!(
                state_availability_boundary.lower_bound
                    == state_availability_boundary.upper_bound
            );
            for main_index in start_main_index + 1..inner.main_chain.len() {
                state_availability_boundary
                    .main_chain
                    .push(inner.arena[inner.main_chain[main_index]].hash);
            }
        }

        if inner.main_chain.len() < DEFERRED_STATE_EPOCH_COUNT as usize {
            return;
        }

        let end_index =
            inner.main_chain.len() - DEFERRED_STATE_EPOCH_COUNT as usize + 1;
        for main_index in start_main_index + 1..end_index {
            let main_arena_index = inner.main_chain[main_index];
            let main_hash = inner.arena[main_arena_index].hash;

            // Ensure that the commitments for the blocks on
            // main_chain after cur_era_stable_genesis are kept in memory.
            if self
                .data_man
                .load_epoch_execution_commitment_from_db(&main_hash)
                .is_none()
            {
                break;
            }
        }

        // Retrieve the most recently executed epoch
        let mut start_compute_epoch_main_index =
            self.get_force_compute_index(inner, start_main_index, end_index);

        // Retrieve the earliest non-executed epoch
        for main_index in start_main_index + 1..end_index {
            let main_arena_index = inner.main_chain[main_index];
            let main_hash = inner.arena[main_arena_index].hash;

            if self
                .data_man
                .get_epoch_execution_commitment(&main_hash)
                .is_none()
            {
                start_compute_epoch_main_index =
                    min(main_index, start_compute_epoch_main_index);
                debug!(
                    "Start compute epoch main index {}, height {}",
                    main_index, inner.arena[main_arena_index].height
                );
                break;
            }
        }

        let snapshot_epoch_count = inner
            .data_man
            .storage_manager
            .get_storage_manager()
            .get_snapshot_epoch_count();
        let need_set_intermediate_trie_root_merkle = false;
            
        self.set_intermediate_trie_root_merkle(
            inner,
            start_compute_epoch_main_index,
            need_set_intermediate_trie_root_merkle,
            snapshot_epoch_count as u64,
        );

        let confirmed_epoch_num = meter.get_confirmed_epoch_num();
        for main_index in start_main_index + 1..end_index {
            let main_arena_index = inner.main_chain[main_index];
            let main_hash = inner.arena[main_arena_index].hash;
            let height = inner.arena[main_arena_index].height;

            let compute_epoch = if main_index >= start_compute_epoch_main_index
            {
                true
            } else {
                false
            };

            if self
                .data_man
                .get_epoch_execution_commitment(&main_hash)
                .is_some()
            {
                self.data_man
                    .state_availability_boundary
                    .write()
                    .upper_bound += 1;
            }

            info!(
                "construct_main_state: index {} height {} compute_epoch {}.",
                main_index, height, compute_epoch,
            );

            if compute_epoch {
                let reward_execution_info = self
                    .executor
                    .get_reward_execution_info(inner, main_arena_index);

                
                self.executor.compute_epoch(
                    EpochExecutionTask::new(
                        main_arena_index,
                        inner,
                        reward_execution_info,
                        true, /* on_local_main */
                        true, /* force_recompute */
                    ),
                    None,
                    false, // Default value for recover_mpt_during_construct_main_state
                );

                // Remove old-main state during start up to save disk,
                // otherwise, all state will be keep till normal phase, this
                // will occupy too many disk
                {
                    let mut confirmed_height = min(confirmed_epoch_num, height);
                    if confirmed_height < DEFERRED_STATE_EPOCH_COUNT {
                        confirmed_height = 0;
                    } else {
                        confirmed_height -= DEFERRED_STATE_EPOCH_COUNT;
                    }

                    self.data_man
                        .storage_manager
                        .get_storage_manager()
                        .maintain_state_confirmed(
                            inner,
                            inner.cur_era_stable_height,
                            inner.inner_conf.era_epoch_count,
                            confirmed_height,
                            &self.data_man.state_availability_boundary,
                        )
                        .expect(&concat!(
                            file!(),
                            ":",
                            line!(),
                            ":",
                            column!()
                        ));
                }
            }
        }

        inner
            .data_man
            .storage_manager
            .get_storage_manager()
            .get_snapshot_manager()
            .get_snapshot_db_manager()
            .clean_snapshot_epoch_id_before_recovered();
    }

    fn get_force_compute_index(
        &self, inner: &mut ConsensusGraphInner, start_main_index: usize,
        end_index: usize,
    ) -> usize {
        let mut force_compute_index = start_main_index + 1;
        let mut epoch_count = 0;
        for main_index in (start_main_index + 1..end_index).rev() {
            let main_arena_index = inner.main_chain[main_index];
            let main_hash = inner.arena[main_arena_index].hash;

            let maybe_epoch_execution_commitment =
                self.data_man.get_epoch_execution_commitment(&main_hash);
            if let Some(commitment) = *maybe_epoch_execution_commitment {
                if self
                    .data_man
                    .storage_manager
                    .get_state_no_commit_inner(
                        StateIndex::new_for_readonly(
                            &main_hash,
                            &commitment.state_root_with_aux_info,
                        ),
                        /* try_open = */ false,
                        false,
                    )
                    .expect("DB Error")
                    .is_some()
                {
                    epoch_count += 1;

                    // force to recompute last 5 epochs in case state database
                    // is not ready in last shutdown
                    if epoch_count > DEFERRED_STATE_EPOCH_COUNT {
                        let reward_execution_info = self
                            .executor
                            .get_reward_execution_info(inner, main_arena_index);

                        let main_block_height = self
                            .data_man
                            .block_header_by_hash(&main_hash)
                            .expect("must exists")
                            .height();

                        // ensure current epoch is new executed whether there
                        // is a fork or not
                        if self.executor.epoch_executed_and_recovered(
                            &main_hash,
                            &inner.get_epoch_block_hashes(main_arena_index),
                            true,
                            &reward_execution_info,
                            main_block_height,
                        ) {
                            force_compute_index = main_index + 1;
                            debug!(
                                "Force compute start index {}",
                                force_compute_index
                            );
                            break;
                        }
                    }
                } else {
                    epoch_count = 0;
                }
            }
        }

        if let Some(height) = None {
            if height > inner.cur_era_stable_height {
                let main_idx = inner.height_to_main_index(height);
                debug!(
                    "force recompute height during constructing main {}",
                    main_idx
                );
                force_compute_index = min(force_compute_index, main_idx);
            }
        }

        force_compute_index
    }

  

    fn set_intermediate_trie_root_merkle(
        &self, inner: &mut ConsensusGraphInner,
        start_compute_epoch_main_index: usize,
        need_set_intermediate_trie_root_merkle: bool,
        snapshot_epoch_count: u64,
    ) {
        let storage_manager =
            inner.data_man.storage_manager.get_storage_manager();
        if !storage_manager
            .storage_conf
            .keep_snapshot_before_stable_checkpoint
            || need_set_intermediate_trie_root_merkle
        {
            let main_arena_index =
                inner.main_chain[start_compute_epoch_main_index - 1];
            let main_hash = inner.arena[main_arena_index].hash;
            let height = inner.arena[main_arena_index].height + 1;

            let intermediate_trie_root_merkle =
                match *self.data_man.get_epoch_execution_commitment(&main_hash)
                {
                    Some(commitment) => {
                        if height % snapshot_epoch_count == 1 {
                            commitment
                                .state_root_with_aux_info
                                .state_root
                                .delta_root
                        } else {
                            commitment
                                .state_root_with_aux_info
                                .state_root
                                .intermediate_delta_root
                        }
                    }
                    None => MERKLE_NULL_NODE,
                };

            debug!("previous main hash {:?} intermediate trie root merkle for next main {:?}", main_hash, intermediate_trie_root_merkle);
            *storage_manager.intermediate_trie_root_merkle.write() =
                Some(intermediate_trie_root_merkle);
        }
    }

    fn set_block_tx_packed(&self, inner: &ConsensusGraphInner, me: usize) {
        if !self.txpool.ready_for_mining() {
            // Skip tx pool operation before catching up.
            return;
        }
        let parent = inner.arena[me].parent;
        if parent == NULL {
            warn!(
                "set_block_tx_packed skips block with empty parent {:?}",
                inner.arena[me].hash
            );
            return;
        }
        let era_genesis_height =
            inner.get_era_genesis_height(inner.arena[parent].height);
        let cur_main_era_block = if inner
            .main_index_to_height(inner.main_chain.len())
            > era_genesis_height
        {
            inner.get_main_block_arena_index(era_genesis_height)
        } else {
            NULL
        };
        let era_block = inner.get_era_genesis_block_with_parent(parent);

        // It's only correct to set tx stale after the block is considered
        // terminal for mining.
        // Note that we conservatively only mark those blocks inside the
        // current main era
        if era_block == cur_main_era_block {
            self.txpool.set_tx_packed(
                &self
                    .data_man
                    .block_by_hash(
                        &inner.arena[me].hash,
                        true, /* update_cache */
                    )
                    .expect("Already checked")
                    .transactions,
            );
        } else {
            warn!("set_block_tx_packed skips block {:?}", inner.arena[me].hash);
        }
    }

    fn delayed_tx_recycle_in_skipped_blocks(
        &self, inner: &mut ConsensusGraphInner, fork_height: u64,
    ) {
        if !self.txpool.ready_for_mining() {
            // Skip tx pool operation before catching up.
            return;
        }
        if inner.main_chain.len() > RECYCLE_TRANSACTION_DELAY as usize {
            let recycle_end_main_index =
                inner.main_chain.len() - RECYCLE_TRANSACTION_DELAY as usize - 1;
            // If the main reorg is deeper than `RECYCLE_TRANSACTION_DELAY`, we
            // will try to recycle all skipped blocks since the
            // forking point.
            let start = min(
                // `fork_height` has been capped by the caller.
                inner.height_to_main_index(fork_height),
                recycle_end_main_index,
            );
            for recycle_main_index in start..=recycle_end_main_index {
                let recycle_arena_index = inner.main_chain[recycle_main_index];
                let skipped_blocks = inner
                    .get_or_compute_skipped_epoch_blocks(recycle_arena_index)
                    .clone();
                for h in &skipped_blocks {
                    self.recycle_tx_in_block(inner, h);
                }
            }
        }
    }

    fn check_mining_adaptive_block(
        &self, _new: usize, _inner: &mut ConsensusGraphInner, _adaptive: bool,
    ) -> bool {
        // Implementation for check_mining_adaptive_block method
        // This method should return true if the block is valid for mining
        // and false otherwise.
        // You can add any additional checks you want to perform here.
        true
    }
}
