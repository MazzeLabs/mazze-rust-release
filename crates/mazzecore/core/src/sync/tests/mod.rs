// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::consensus::ConsensusGraphTrait;
use crate::sync::{
    utils::{create_simple_block_impl, initialize_synchronization_graph},
    SynchronizationGraphNode,
};
use mazze_types::{BigEndianHash, H256, U256};
use primitives::Block;
use std::{
    fs,
    sync::Arc,
    thread::sleep,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[test]
fn test_remove_expire_blocks() {
    {
        let (sync, _, _, _) = initialize_synchronization_graph(
            "./test.db/",
            1,
            1,
            1,
            1,
            50000,
            H256::default(),
        );
        // test initialization
        {
            let inner = sync.inner.read();
            assert!(inner.arena.len() == 1);
            assert!(inner.hash_to_arena_indices.len() == 1);
            assert!(inner.not_ready_blocks_frontier.len() == 0);
        }

        // prepare graph data
        {
            let mut blocks: Vec<Block> = Vec::new();
            let parent: Vec<i64> =
                vec![-1, 0, 0, 0, 3, 100, 2, 100, 4, 100, 9, 7];
            let childrens: Vec<Vec<usize>> = vec![
                vec![1, 2, 3],
                vec![],
                vec![6],
                vec![4],
                vec![8],
                vec![],
                vec![],
                vec![11],
                vec![],
                vec![10],
                vec![],
                vec![],
            ];
            let referrers: Vec<Vec<usize>> = vec![
                vec![],
                vec![4],
                vec![],
                vec![],
                vec![6],
                vec![4],
                vec![],
                vec![4],
                vec![],
                vec![],
                vec![11],
                vec![],
            ];
            let referee: Vec<Vec<usize>> = vec![
                vec![],
                vec![],
                vec![],
                vec![],
                vec![1, 5, 7],
                vec![],
                vec![4],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![10],
            ];
            let graph_status = vec![4, 4, 4, 4, 2, 1, 1, 1, 1, 1, 1, 1];
            for i in 0..12 {
                let parent_hash = {
                    if parent[i as usize] == -1 {
                        H256::default()
                    } else if parent[i as usize] >= i {
                        BigEndianHash::from_uint(&U256::from(100 + i as usize))
                    } else {
                        blocks[parent[i as usize] as usize].hash()
                    }
                };
                let (_, block) = create_simple_block_impl(
                    parent_hash,
                    vec![],
                    0,
                    U256::from(i),
                    U256::from(10),
                    1,
                    false,
                );
                blocks.push(block);
            }

            let mut inner = sync.inner.write();
            for i in 1..12 {
                let parent_index = if parent[i] > 12 {
                    !0 as usize
                } else {
                    parent[i] as usize
                };
                let me = inner.arena.insert(SynchronizationGraphNode {
                    graph_status: graph_status[i as usize],
                    block_ready: false,
                    sent_to_consensus: false,
                    parent_reclaimed: false,
                    parent: parent_index,
                    children: childrens[i as usize].clone(),
                    referees: referee[i as usize].clone(),
                    pending_referee_count: 0,
                    referrers: referrers[i as usize].clone(),
                    block_header: Arc::new(blocks[i].block_header.clone()),
                    last_update_timestamp: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        - 100,
                });
                assert_eq!(me, i);
                inner
                    .hash_to_arena_indices
                    .insert(blocks[i as usize].hash(), me);
                if graph_status[i as usize] != 4
                    && (parent_index > 12 || graph_status[parent_index] == 4)
                {
                    let status = {
                        if parent_index > 12 {
                            5
                        } else {
                            graph_status[parent_index]
                        }
                    };
                    println!(
                        "insert {} parent {} parent_status {}",
                        i, parent_index, status
                    );
                    inner.not_ready_blocks_frontier.insert(me);
                }
            }

            println!(
                "not_ready_blocks_frontier={:?}",
                inner.not_ready_blocks_frontier.get_frontier()
            );
            assert!(inner.arena.len() == 12);
            assert!(inner.hash_to_arena_indices.len() == 12);
            assert!(inner.not_ready_blocks_frontier.len() == 5);
            assert!(inner.not_ready_blocks_frontier.contains(&(4 as usize)));
            assert!(inner.not_ready_blocks_frontier.contains(&(5 as usize)));
            assert!(inner.not_ready_blocks_frontier.contains(&(6 as usize)));
            assert!(inner.not_ready_blocks_frontier.contains(&(7 as usize)));
            assert!(inner.not_ready_blocks_frontier.contains(&(9 as usize)));
        }

        // not expire any blocks
        {
            sync.remove_expire_blocks(1000 /* expire_time */);
            let inner = sync.inner.read();
            assert!(inner.arena.len() == 12);
            assert!(inner.hash_to_arena_indices.len() == 12);
            assert!(inner.not_ready_blocks_frontier.len() == 5);
            assert!(inner.not_ready_blocks_frontier.contains(&(4 as usize)));
            assert!(inner.not_ready_blocks_frontier.contains(&(5 as usize)));
            assert!(inner.not_ready_blocks_frontier.contains(&(6 as usize)));
            assert!(inner.not_ready_blocks_frontier.contains(&(7 as usize)));
            assert!(inner.not_ready_blocks_frontier.contains(&(9 as usize)));
        }

        // expire [10, 11]
        {
            let mut inner = sync.inner.write();
            inner.arena[10].last_update_timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                - 1000;
        }
        {
            sync.remove_expire_blocks(500 /* expire_time */);
            let inner = sync.inner.read();
            assert!(inner.arena.len() == 10);
            assert!(inner.hash_to_arena_indices.len() == 10);
            assert!(inner.not_ready_blocks_frontier.len() == 5);
            assert!(inner.not_ready_blocks_frontier.contains(&(4 as usize)));
            assert!(inner.not_ready_blocks_frontier.contains(&(5 as usize)));
            assert!(inner.not_ready_blocks_frontier.contains(&(6 as usize)));
            assert!(inner.not_ready_blocks_frontier.contains(&(7 as usize)));
            assert!(inner.not_ready_blocks_frontier.contains(&(9 as usize)));
        }

        // expire [9, 7]
        {
            let mut inner = sync.inner.write();
            inner.arena[7].last_update_timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                - 1000;
            inner.arena[9].last_update_timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                - 1000;
        }
        {
            sync.remove_expire_blocks(500 /* expire_time */);
            let inner = sync.inner.read();
            assert!(inner.arena.len() == 5);
            assert!(inner.hash_to_arena_indices.len() == 5);
            assert!(inner.not_ready_blocks_frontier.len() == 1);
            assert!(inner.not_ready_blocks_frontier.contains(&(5 as usize)));
        }
    }

    let mut retry = 3;
    while let Err(e) = fs::remove_dir_all("./test.db") {
        println!("failed to remove directory test.db, err = {:?}", e);
        assert!(retry > 0);
        retry -= 1;
        sleep(Duration::from_millis(300));
    }
}

/// Wait until the consensus worker has drained everything the sync graph
/// handed to it.
fn wait_consensus_drained(sync: &crate::sync::SynchronizationGraph) {
    let start = std::time::Instant::now();
    while sync.is_consensus_worker_busy() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "consensus worker did not drain"
        );
        sleep(Duration::from_millis(5));
    }
}

fn remove_test_db(dir: &str) {
    let mut retry = 3;
    while let Err(e) = fs::remove_dir_all(dir) {
        println!("failed to remove directory {}, err = {:?}", dir, e);
        if retry == 0 {
            return;
        }
        retry -= 1;
        sleep(Duration::from_millis(300));
    }
}

/// Build `n` empty blocks on a single chain above the genesis, with the base
/// price the sync graph expects (it is checked at BLOCK_GRAPH_READY).
fn simple_chain(
    sync: &crate::sync::SynchronizationGraph, genesis: &Block, n: usize,
) -> Vec<Block> {
    use mazze_parameters::{
        consensus::GENESIS_GAS_LIMIT, consensus_internal::ELASTICITY_MULTIPLIER,
    };
    use mazze_types::{address_util::AddressUtil, Address, SpaceMap};
    use primitives::{block_header::compute_next_price_tuple, BlockHeaderBuilder};

    let machine = sync.machine();
    let params = machine.params();
    let mut blocks: Vec<Block> = Vec::new();
    let mut parent = genesis.hash();
    let mut parent_price: Option<SpaceMap<U256>> = None;
    for i in 0..n {
        let height = genesis.block_header.height() + 1 + i as u64;
        let gas_limit = U256::from(GENESIS_GAS_LIMIT);
        let espace_limit = if params.can_pack_evm_transaction(height) {
            gas_limit * 5 / 10
        } else {
            U256::zero()
        };
        let gas_target = SpaceMap::new(gas_limit * 9 / 10, espace_limit)
            .map_all(|x| x / ELASTICITY_MULTIPLIER);
        let parent_base_price = if height == 1 {
            params.init_base_price()
        } else {
            parent_price.unwrap()
        };
        let price = SpaceMap::zip4(
            gas_target,
            SpaceMap::default(),
            parent_base_price,
            params.min_base_price(),
        )
        .map_all(compute_next_price_tuple);
        let mut author = Address::zero();
        author.set_user_account_type_bits();
        let mut header = BlockHeaderBuilder::new()
            .with_parent_hash(parent)
            .with_height(height)
            .with_gas_limit(gas_limit)
            .with_nonce(U256::from(1000 + i))
            .with_difficulty(U256::from(10))
            .with_author(author)
            .with_base_price(Some(price))
            .build();
        header.compute_hash();
        header.pow_hash = Some(crate::pow::pow_quality_to_hash(
            &U256::from(10),
            &header.nonce(),
        ));
        let block = Block::new(header, vec![]);
        parent = block.hash();
        parent_price = Some(price);
        blocks.push(block);
    }
    blocks
}

/// A block delivered to consensus twice (once at HEADER_GRAPH_READY in a
/// header-to-consensus phase, again at BLOCK_GRAPH_READY when its body
/// arrives -- body sync runs in every phase, §5.20) must be inserted into the
/// consensus arena exactly once. A second `ConsensusGraphInner::insert`
/// creates a shadow arena node and re-points `hash_to_arena_indices` to it;
/// once an era checkpoint lands on the original lineage the shadow is turned
/// into out-era "legacy" (era_block = NULL) and every later descendant is
/// inserted as an out-era stub (m6, 2026-10-17: consensus frozen at 484767).
#[test]
fn test_header_then_body_is_delivered_to_consensus_once() {
    let db = "./test_dup_delivery.db/";
    {
        let (sync, consensus, _, genesis) = initialize_synchronization_graph(
            db,
            1,
            1,
            1,
            1,
            50000,
            H256::default(),
        );
        let blocks =
            simple_chain(&sync, &genesis, 4);

        // Header phase: every header goes to consensus at HGR.
        for b in &blocks {
            let mut h = b.block_header.clone();
            sync.insert_block_header(
                &mut h, false, /* need_to_verify */
                true,  /* bench_mode */
                true,  /* insert_to_consensus */
                true,  /* persistent */
            );
        }
        wait_consensus_drained(&sync);
        // Bodies arrive afterwards (still not locked for catch-up).
        for b in &blocks {
            sync.insert_block(
                b.clone(),
                false, /* need_to_verify */
                true,  /* persistent */
                false, /* recover_from_db */
            );
        }
        wait_consensus_drained(&sync);

        let inner = consensus.inner.read();
        // genesis + 4 blocks, no shadow copies.
        assert_eq!(
            inner.arena.len(),
            1 + blocks.len(),
            "duplicate consensus delivery created shadow arena nodes"
        );
        for b in &blocks {
            let idx = inner.hash_to_arena_indices[&b.hash()];
            assert_ne!(inner.arena[idx].era_block(), !0usize);
        }
    }
    remove_test_db(db);
}

/// A block re-delivered to consensus after a main block has already taken it
/// into its epoch must not be executed again in a later epoch. Without the
/// duplicate guard in `ConsensusNewBlockHandler::on_new_block` the repeat
/// became a shadow arena node; a later block built on it (here `t`, child of
/// `s`) reached the shadow, which is not in the past set of the previous main
/// block, so the shadow joined the next epoch too: the epoch held one block
/// more, every later block number shifted and the state diverged. This is how
/// m2's from-genesis replay on 9d6249e (no 99b151e) left the network at epoch
/// 99136 (blocks 0x3bc18a60 / 0x63bb4afe executed in epochs 99133 / 99132 and
/// again in 99136; receipts root of 4 empty blocks instead of 2).
#[test]
fn test_redelivered_block_is_not_executed_in_a_second_epoch() {
    use crate::sync::utils::create_simple_block_impl;
    let db = "./test_redelivered_epoch.db/";
    {
        let (sync, consensus, _, genesis) = initialize_synchronization_graph(
            db,
            1,
            1,
            1,
            1,
            50000,
            H256::default(),
        );
        let diff = U256::from(10);
        let block = |parent: H256, refs: Vec<H256>, height: u64, nonce: u64| {
            create_simple_block_impl(
                parent,
                refs,
                height,
                U256::from(nonce),
                diff,
                1,
                false,
            )
            .1
        };
        let headers = |blocks: &[&Block]| {
            for b in blocks {
                let mut h = b.block_header.clone();
                sync.insert_block_header(&mut h, false, true, true, true);
            }
            wait_consensus_drained(&sync);
        };

        // Main chain g <- a1 <- a2 <- a3; side block s (child of g) is
        // referenced by a2, its child t by a3.
        let g = genesis.hash();
        let a1 = block(g, vec![], 1, 1);
        let s = block(g, vec![], 1, 2);
        let a2 = block(a1.hash(), vec![s.hash()], 2, 3);
        headers(&[&a1, &s, &a2]);

        // The repeat delivery (header-graph-ready, then graph-ready).
        consensus.on_new_block(&s.hash());

        let t = block(s.hash(), vec![], 2, 4);
        let a3 = block(a2.hash(), vec![t.hash()], 3, 5);
        headers(&[&t, &a3]);

        let inner = consensus.inner.read();
        let epoch = |main: &Block| -> Vec<H256> {
            let idx = inner.hash_to_arena_indices[&main.hash()];
            inner
                .get_ordered_executable_epoch_blocks(idx)
                .iter()
                .map(|i| inner.arena[*i].hash)
                .collect()
        };
        assert_eq!(epoch(&a1), vec![a1.hash()]);
        assert_eq!(epoch(&a2), vec![s.hash(), a2.hash()]);
        assert_eq!(
            epoch(&a3),
            vec![t.hash(), a3.hash()],
            "a block of an earlier epoch was executed again"
        );
        let past = |b: &Block| {
            let idx = inner.hash_to_arena_indices[&b.hash()];
            inner.arena[idx].past_num_blocks()
        };
        // Block numbers stay consecutive: 2 blocks in a3's epoch.
        assert_eq!(past(&a3), past(&a2) + 2);
        assert_eq!(
            inner.arena.len(),
            6,
            "duplicate consensus delivery created a shadow arena node"
        );
    }
    remove_test_db(db);
}

/// A block that becomes HEADER_GRAPH_READY through a call that does not
/// insert into consensus (e.g. a body response for a header-less block,
/// `insert_to_consensus = false`) must still reach consensus before a child
/// that is sent at HEADER_GRAPH_READY; otherwise the child is processed while
/// its parent is absent and is inserted as an out-era stub forever.
#[test]
fn test_unsent_parent_is_delivered_before_child() {
    let db = "./test_unsent_parent.db/";
    {
        let (sync, consensus, _, genesis) = initialize_synchronization_graph(
            db,
            1,
            1,
            1,
            1,
            50000,
            H256::default(),
        );
        let blocks =
            simple_chain(&sync, &genesis, 3);

        let mut h0 = blocks[0].block_header.clone();
        sync.insert_block_header(&mut h0, false, true, false, true);
        for b in &blocks[1..] {
            let mut h = b.block_header.clone();
            sync.insert_block_header(&mut h, false, true, true, true);
        }
        wait_consensus_drained(&sync);

        let inner = consensus.inner.read();
        for b in &blocks {
            let idx = *inner
                .hash_to_arena_indices
                .get(&b.hash())
                .expect("block must be in consensus");
            assert_ne!(
                inner.arena[idx].era_block(),
                !0usize,
                "block {:?} was inserted as an out-era stub",
                b.hash()
            );
        }
    }
    remove_test_db(db);
}

/// End-to-end shape of the m6 freeze (2026-10-17). In a header-to-consensus
/// phase every header is handed to consensus at HEADER_GRAPH_READY; bodies
/// (body sync runs in every phase) trail behind and each one handed the block
/// to consensus AGAIN at BLOCK_GRAPH_READY. Consensus inserted a second
/// ("shadow") arena node per duplicate and re-pointed the hash at it. An era
/// checkpoint on the original lineage then turned the shadow into out-era
/// legacy (era_block = NULL) while the hashes still mapped to it, and from the
/// moment the body wave caught up with the tip every new block was inserted
/// as an out-era stub: the pivot froze for good. (With the tiny parameters
/// used here the pre-fix corruption instead trips the timer-chain assertion
/// in `make_checkpoint_at` and kills the consensus worker.)
#[test]
fn test_pivot_advances_when_bodies_trail_headers_across_a_checkpoint() {
    let db = "./test_dup_checkpoint.db/";
    {
        let (sync, consensus, _, genesis) = initialize_synchronization_graph(
            db,
            1,
            1,
            1,
            1,
            10, /* era_epoch_count */
            H256::default(),
        );
        let blocks = simple_chain(&sync, &genesis, 150);
        let headers = |range: std::ops::Range<usize>| {
            for b in &blocks[range] {
                let mut h = b.block_header.clone();
                sync.insert_block_header(&mut h, false, true, true, true);
            }
            wait_consensus_drained(&sync);
        };
        let bodies = |range: std::ops::Range<usize>| {
            for b in &blocks[range] {
                sync.insert_block(b.clone(), false, true, false);
            }
            wait_consensus_drained(&sync);
        };
        headers(0..60);
        bodies(0..30); // body wave trails the header tip
        headers(60..120); // pivot (and era checkpoints) move on
        bodies(30..120); // body wave catches up with the tip
        headers(120..150); // new blocks at the tip

        let inner = consensus.inner.read();
        let out_era = inner
            .arena
            .iter()
            .filter(|(_, n)| n.era_block() == !0usize)
            .count();
        println!(
            "era_genesis_height={} arena={} out_era={} best_epoch={}",
            inner.get_cur_era_genesis_height(),
            inner.arena.len(),
            out_era,
            consensus.best_epoch_number()
        );
        assert!(inner.get_cur_era_genesis_height() > 0, "no checkpoint formed");
        drop(inner);
        assert_eq!(consensus.best_epoch_number(), 150);
    }
    remove_test_db(db);
}

/// In a header-to-consensus phase a block is in consensus at
/// HEADER_GRAPH_READY, but it stays in `not_ready_blocks_frontier` until its
/// body arrives (body sync runs in every phase, §5.20). Expiry must not remove
/// such a block, nor the chain above it: m2's from-genesis archive replay
/// (2026-10-20) lost every sync-graph node above height 9374 two hours into
/// header sync, then rebuilt consensus from the gutted sync graph at the end
/// of body fill and froze at epoch 9374 for good.
#[test]
fn test_expiry_keeps_blocks_consensus_holds() {
    use crate::sync::utils::create_simple_block_impl;
    let db = "./test_expiry_keeps_consensus.db/";
    {
        let (sync, consensus, _, genesis) = initialize_synchronization_graph(
            db,
            1,
            1,
            1,
            1,
            50000,
            H256::default(),
        );
        let blocks = simple_chain(&sync, &genesis, 4);

        // Header phase: every header goes to consensus at HGR.
        for b in &blocks {
            let mut h = b.block_header.clone();
            sync.insert_block_header(&mut h, false, true, true, true);
        }
        // An orphan header whose parent never arrives: not in consensus.
        let (_, orphan) = create_simple_block_impl(
            H256::from_low_u64_be(0xdead),
            vec![],
            7,
            U256::from(77),
            U256::from(10),
            1,
            false,
        );
        let mut oh = orphan.block_header.clone();
        sync.insert_block_header(&mut oh, false, true, true, true);
        wait_consensus_drained(&sync);

        // Only the first body arrives: its HGR child joins the frontier.
        sync.insert_block(blocks[0].clone(), false, true, false);
        wait_consensus_drained(&sync);
        {
            let mut inner = sync.inner.write();
            let i1 = inner.hash_to_arena_indices[&blocks[1].hash()];
            assert!(
                inner.not_ready_blocks_frontier.contains(&i1),
                "test setup: the HGR child must be a not-ready frontier block"
            );
            let old = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                - 10_000;
            let idx: Vec<usize> =
                inner.hash_to_arena_indices.values().copied().collect();
            for i in idx {
                inner.arena[i].last_update_timestamp = old;
            }
        }

        sync.remove_expire_blocks(500 /* expire_time */);

        let inner = sync.inner.read();
        for b in &blocks {
            assert!(
                inner.hash_to_arena_indices.contains_key(&b.hash()),
                "block {:?} held by consensus was expired from the sync graph",
                b.hash()
            );
            assert!(
                sync.data_man.block_header_by_hash(&b.hash()).is_some(),
                "header of {:?} held by consensus was deleted",
                b.hash()
            );
            assert!(consensus.contains_block(&b.hash()));
        }
        assert!(
            !inner.hash_to_arena_indices.contains_key(&orphan.hash()),
            "a stale orphan must still expire"
        );
    }
    remove_test_db(db);
}

/// Cancelling a body request must not remove a block consensus holds, or the
/// chain above it.
#[test]
fn test_cancelled_request_keeps_blocks_consensus_holds() {
    let db = "./test_cancel_keeps_consensus.db/";
    {
        let (sync, _, _, genesis) = initialize_synchronization_graph(
            db,
            1,
            1,
            1,
            1,
            50000,
            H256::default(),
        );
        let blocks = simple_chain(&sync, &genesis, 3);
        for b in &blocks {
            let mut h = b.block_header.clone();
            sync.insert_block_header(&mut h, false, true, true, true);
        }
        wait_consensus_drained(&sync);

        let mut to_remove = std::collections::HashSet::new();
        to_remove.insert(blocks[0].hash());
        sync.remove_blocks_and_future(&to_remove);

        let inner = sync.inner.read();
        for b in &blocks {
            assert!(inner.hash_to_arena_indices.contains_key(&b.hash()));
            assert!(sync.data_man.block_header_by_hash(&b.hash()).is_some());
        }
    }
    remove_test_db(db);
}
