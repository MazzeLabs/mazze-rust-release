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
        let gas_target =
            SpaceMap::new(gas_limit * 9 / 10, espace_limit, U256::zero())
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

/// A block's shielded section survives the database: after the block is
/// written and read back without the cache, the section is still there.
#[test]
fn shielded_section_survives_the_block_database() {
    use crate::{pow::PowComputer, sync::utils::initialize_data_manager};
    use mazze_executor::machine::VmFactory;
    use primitives::{shielded::ShieldedTxBody, BlockHeaderBuilder};

    let dir = "./test_shielded_body.db/";
    let _ = fs::remove_dir_all(dir);
    {
        let (data_man, genesis) = initialize_data_manager(
            dir,
            Arc::new(PowComputer::new(H256::zero())),
            VmFactory::new(1024),
        );
        let shielded: Vec<Arc<ShieldedTxBody>> = (0..3)
            .map(|i| Arc::new(ShieldedTxBody::sample_for_tests(i)))
            .collect();
        let header = BlockHeaderBuilder::new()
            .with_parent_hash(genesis.hash())
            .with_height(1)
            .build();
        let witness: Vec<Arc<Vec<u8>>> =
            (0..3).map(|i| Arc::new(vec![i as u8; 1000])).collect();
        let block = Arc::new(
            Block::new(header, vec![])
                .with_shielded(shielded.clone())
                .with_witness(witness.clone()),
        );
        data_man.insert_block(block.clone(), true);

        let reloaded = data_man
            .db_manager
            .block_from_db(&block.hash())
            .expect("block in the database");
        assert_eq!(reloaded.shielded, shielded);
        assert_eq!(reloaded.witness, witness, "the witness is retained");
        assert_eq!(reloaded.hash(), block.hash());

        // Pruning below the block's height keeps it; above deletes it.
        assert_eq!(data_man.db_manager.prune_witnesses_below(1), 0);
        assert_eq!(data_man.db_manager.prune_witnesses_below(2), 1);
        let reloaded = data_man.db_manager.block_from_db(&block.hash()).unwrap();
        assert_eq!(reloaded.shielded, shielded, "the body stays");
        assert!(reloaded.witness.is_empty());
    }
    let _ = fs::remove_dir_all(dir);
}

/// The shielded history is kept per (epoch, pivot): a reorg that switches
/// the pivot back without re-executing still finds its own entry, and an
/// epoch whose pivot is unknown (below a fast-sync anchor) uses the only one.
#[test]
fn shielded_history_is_kept_per_pivot() {
    use crate::{
        block_data_manager::ShieldedHistoryEntry, pow::PowComputer,
        sync::utils::initialize_data_manager,
    };
    use mazze_executor::{
        machine::VmFactory,
        shielded_engine::{ShieldedOutcome, SkipReason},
    };
    use primitives::shielded::ShieldedTxBody;

    let dir = "./test_shielded_history.db/";
    let _ = fs::remove_dir_all(dir);
    {
        let (data_man, _) = initialize_data_manager(
            dir,
            Arc::new(PowComputer::new(H256::zero())),
            VmFactory::new(1024),
        );
        let entry = |seed: u32, outcome: ShieldedOutcome| ShieldedHistoryEntry {
            bodies: vec![ShieldedTxBody::sample_for_tests(seed)],
            outcomes: vec![outcome],
        };
        let (a, b) = (H256::repeat_byte(0xa), H256::repeat_byte(0xb));
        let ea = entry(1, ShieldedOutcome::Applied { positions: [4, 5] });
        let eb = entry(2, ShieldedOutcome::Skipped(SkipReason::UnknownAnchor));
        data_man.insert_shielded_history(20, &a, &ea);
        assert_eq!(data_man.shielded_history(20, None), Some((a, ea.clone())));
        data_man.insert_shielded_history(20, &b, &eb);
        assert_eq!(data_man.shielded_history(20, Some(&a)), Some((a, ea.clone())));
        assert_eq!(data_man.shielded_history(20, Some(&b)), Some((b, eb)));
        assert_eq!(data_man.shielded_history(20, None), None, "ambiguous");
        assert_eq!(data_man.shielded_history(30, Some(&a)), None);
        assert_eq!(data_man.shielded_history(19, None), None);
        assert_eq!(data_man.shielded_history(21, None), None);
    }
    let _ = fs::remove_dir_all(dir);
}

/// Spec A6 / lesson Z-3: a block whose shielded proof does not verify is
/// rejected on every path into the graph: from a peer (gossip, catch-up,
/// fast-sync replay all insert with verification) and as this node's own
/// mined block (inserted without the other checks). Only a block re-read
/// from this node's own database, verified when first accepted, skips it.
/// A reorg cannot bring the block back: it never entered the graph.
#[test]
fn an_invalid_proof_is_rejected_on_every_path() {
    use crate::{
        pow::PowComputer,
        sync::{
            synchronization_graph::BlockInsertionResult,
            utils::{initialize_data_manager, initialize_synchronization_graph_with_params},
        },
        verification::compute_block_transactions_root,
    };
    use mazze_executor::{machine::VmFactory, spec::CommonParams};

    let db = "./test_invalid_proof_paths.db/";
    let _ = fs::remove_dir_all(db);
    {
        let vm = VmFactory::new(1024 * 32);
        let pow = Arc::new(PowComputer::new(H256::default()));
        let (data_man, genesis) = initialize_data_manager(db, pow.clone(), vm.clone());
        let mut params = CommonParams::default();
        params.shielded.activation_height = 0;
        params.shielded.slot_epochs = 1;
        let fee = params.shielded.tx_fee;
        let (sync, _consensus) = initialize_synchronization_graph_with_params(
            data_man, 1, 1, 1, 1, 50000, pow, vm, params,
        );
        let chain = sync.consensus.best_chain_id().in_shielded_space();
        let (good_body, proof) =
            mazze_shielded_pq::testkit::payment(chain, 100 * fee, fee, 1);
        // The proof still hashes to the body, but proves other outputs.
        let mut bad_body = good_body.clone();
        bad_body.cm_out.swap(0, 1);

        let base = simple_chain(&sync, &genesis, 2);
        // A child of `base[1]` at height 3 with the section, priced the way
        // `simple_chain` prices its blocks.
        let with_section = |nonce: u64, body: &primitives::shielded::ShieldedTxBody| {
            use mazze_parameters::{
                consensus::GENESIS_GAS_LIMIT, consensus_internal::ELASTICITY_MULTIPLIER,
            };
            use mazze_types::{address_util::AddressUtil, Address, SpaceMap};
            use primitives::{block_header::compute_next_price_tuple, BlockHeaderBuilder};
            let machine = sync.machine();
            let params = machine.params();
            let parent = &base[1].block_header;
            let gas_limit = U256::from(GENESIS_GAS_LIMIT);
            let espace_limit = if params.can_pack_evm_transaction(3) {
                gas_limit * 5 / 10
            } else {
                U256::zero()
            };
            let gas_target = SpaceMap::new(gas_limit * 9 / 10, espace_limit, U256::zero())
                .map_all(|x| x / ELASTICITY_MULTIPLIER);
            let price = SpaceMap::zip4(
                gas_target,
                SpaceMap::default(),
                parent.base_price().unwrap(),
                params.min_base_price(),
            )
            .map_all(compute_next_price_tuple);
            let shielded = vec![Arc::new(body.clone())];
            let mut author = Address::zero();
            author.set_user_account_type_bits();
            let mut header = BlockHeaderBuilder::new()
                .with_parent_hash(base[1].hash())
                .with_height(3)
                .with_gas_limit(gas_limit)
                .with_nonce(U256::from(nonce))
                .with_difficulty(U256::from(10))
                .with_author(author)
                .with_base_price(Some(price))
                .with_transactions_root(compute_block_transactions_root(&vec![], &shielded))
                .build();
            header.compute_hash();
            header.pow_hash =
                Some(crate::pow::pow_quality_to_hash(&U256::from(10), &header.nonce()));
            Block::new(header, vec![])
                .with_shielded(shielded)
                .with_witness(vec![Arc::new(proof.clone())])
        };
        for b in &base {
            let mut h = b.block_header.clone();
            sync.insert_block_header(&mut h, false, true, true, true);
            sync.insert_block(b.clone(), false, true, false);
        }
        let insert = |block: &Block, need_to_verify: bool, recover_from_db: bool| {
            let mut h = block.block_header.clone();
            sync.insert_block_header(&mut h, false, true, true, true);
            sync.insert_block(block.clone(), need_to_verify, true, recover_from_db)
        };
        let invalid = |r: BlockInsertionResult| matches!(r, BlockInsertionResult::Invalid);

        // Three blocks on the same parent, each with the invalid section.
        assert!(invalid(insert(&with_section(5000, &bad_body), true, false)), "from a peer");
        assert!(
            invalid(insert(&with_section(5001, &bad_body), false, false)),
            "mined by this node"
        );
        let from_db = with_section(5002, &bad_body);
        sync.data_man.insert_block(Arc::new(from_db.clone()), true);
        assert!(
            !invalid(insert(&from_db, true, true)),
            "re-read from this node's database: verified when first accepted"
        );
        // Control: the honest section is accepted from a peer.
        assert!(!invalid(insert(&with_section(6000, &good_body), true, false)));
    }
    let _ = fs::remove_dir_all(db);
}
