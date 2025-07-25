use std::{
    collections::HashMap, path::Path, str::FromStr, sync::Arc, time::Duration,
};

use parking_lot::Mutex;
use threadpool::ThreadPool;

use mazze_internal_common::ChainIdParamsInner;
use mazze_parameters::{
    block::{MAX_BLOCK_SIZE_IN_BYTES, REFEREE_DEFAULT_BOUND},
    consensus::{GENESIS_GAS_LIMIT, TRANSACTION_DEFAULT_EPOCH_BOUND},
    tx_pool::TXPOOL_DEFAULT_NONCE_BITS,
    WORKER_COMPUTATION_PARALLELISM,
};
use mazze_storage::{StorageConfiguration, StorageManager};
use mazze_types::{
    address_util::AddressUtil, Address, AddressSpaceUtil, AllChainID, H256,
    U256,
};
use primitives::{Block, BlockHeaderBuilder};

use crate::{
    block_data_manager::{BlockDataManager, DataManagerConfiguration, DbType},
    cache_config::CacheConfig,
    consensus::{
        consensus_inner::consensus_executor::ConsensusExecutionConfiguration,
        ConsensusConfig,
    },
    db::NUM_COLUMNS,
    genesis_block::genesis_block,
    pow::{self, PowComputer, ProofOfWorkConfig},
    statistics::Statistics,
    sync::{SyncGraphConfig, SynchronizationGraph},
    transaction_pool::TxPoolConfig,
    verification::VerificationConfig,
    ConsensusGraph, NodeType, Notifications, TransactionPool,
};
use mazze_executor::{
    machine::{new_machine_with_builtin, VmFactory},
    spec::CommonParams,
};

pub fn create_simple_block_impl(
    parent_hash: H256, ref_hashes: Vec<H256>, height: u64, nonce: U256,
    diff: U256, block_weight: u32, adaptive: bool,
) -> (H256, Block) {
    let mut b = BlockHeaderBuilder::new();
    let mut author = Address::zero();
    author.set_user_account_type_bits();
    let mut header = b
        .with_parent_hash(parent_hash)
        .with_height(height)
        .with_referee_hashes(ref_hashes)
        .with_gas_limit(GENESIS_GAS_LIMIT.into())
        .with_nonce(nonce)
        .with_difficulty(diff)
        .with_adaptive(adaptive)
        .with_author(author)
        .build();
    header.compute_hash();
    let pow_quality = if block_weight > 1 {
        diff * block_weight
    } else {
        diff
    };
    // To convert pow_quality back to pow_hash can be inaccurate, but it should
    // be okay in tests.
    header.pow_hash =
        Some(pow::pow_quality_to_hash(&pow_quality, &header.nonce()));
    // println!("simple_block: difficulty={:?} pow_hash={:?} pow_quality={}",
    // pow_quality, header.pow_hash,
    // pow::pow_hash_to_quality(&header.pow_hash.unwrap(), &header.nonce()));
    let block = Block::new(header, vec![]);
    (block.hash(), block)
}

pub fn create_simple_block(
    sync: Arc<SynchronizationGraph>, parent_hash: H256, ref_hashes: Vec<H256>,
    height: u64, block_weight: u32, adaptive: bool,
) -> (H256, Block) {
    //    sync.consensus.wait_for_generation(&parent_hash);
    // let parent_header = sync.block_header_by_hash(&parent_hash).unwrap();
    //    let exp_diff = sync.expected_difficulty(&parent_hash);
    //    assert!(
    //        exp_diff == U256::from(10),
    //        "Difficulty hike in bench is not supported yet!"
    //    );
    // Note that because we do not fill the timestamp, it should keep at the
    // minimum difficulty of 10.
    let exp_diff = U256::from(10);
    let nonce = U256::from(sync.block_count() as u64 + 1);
    create_simple_block_impl(
        parent_hash,
        ref_hashes,
        height,
        nonce,
        exp_diff,
        block_weight,
        adaptive,
    )
}

pub fn initialize_data_manager(
    db_dir: &str, dbtype: DbType, pow: Arc<PowComputer>, vm: VmFactory,
) -> (Arc<BlockDataManager>, Arc<Block>) {
    let ledger_db = db::open_database(
        db_dir,
        &db::db_config(
            Path::new(db_dir),
            Some(128),
            db::DatabaseCompactionProfile::default(),
            NUM_COLUMNS,
            false,
        ),
    )
    .map_err(|e| format!("Failed to open database {:?}", e))
    .unwrap();

    let worker_thread_pool = Arc::new(Mutex::new(ThreadPool::with_name(
        "Tx Recover".into(),
        WORKER_COMPUTATION_PARALLELISM,
    )));

    let storage_manager = Arc::new(
        StorageManager::new(StorageConfiguration::new_default(
            db_dir,
            mazze_parameters::consensus::SNAPSHOT_EPOCHS_CAPACITY,
            mazze_parameters::consensus::ERA_DEFAULT_EPOCH_COUNT,
        ))
        .expect("Failed to initialize storage."),
    );

    let mut genesis_accounts = HashMap::new();
    genesis_accounts.insert(
        Address::from_str("1000000000000000000000000000000000000008")
            .unwrap()
            .with_native_space(),
        U256::from(0),
    );

    let machine = Arc::new(new_machine_with_builtin(Default::default(), vm));

    let genesis_block = Arc::new(genesis_block(
        &storage_manager,
        genesis_accounts,
        Address::from_str("1000000000000000000000000000000000000008").unwrap(),
        U256::from(10),
        machine.clone(),
        false, /* need_to_execute */
        None,
    ));

    let data_man = Arc::new(BlockDataManager::new(
        CacheConfig::default(),
        genesis_block.clone(),
        ledger_db.clone(),
        storage_manager,
        worker_thread_pool,
        DataManagerConfiguration::new(
            false,                          /* do not persist transaction
                                             * address */
            false, /* do not persist block number index */
            Duration::from_millis(300_000), /* max cached tx count */
            dbtype,
        ),
        pow,
    ));
    (data_man, genesis_block)
}

pub fn initialize_synchronization_graph_with_data_manager(
    data_man: Arc<BlockDataManager>, _beta: u64, _h: u64, _tcr: u64, _tcb: u64,
    _era_epoch_count: u64, pow: Arc<PowComputer>, vm: VmFactory,
) -> (Arc<SynchronizationGraph>, Arc<ConsensusGraph>) {
    let params = CommonParams::default();
    let machine = Arc::new(new_machine_with_builtin(params, vm));

    let verification_config = VerificationConfig::new(
        true, /* test_mode */
        REFEREE_DEFAULT_BOUND,
        MAX_BLOCK_SIZE_IN_BYTES,
        TRANSACTION_DEFAULT_EPOCH_BOUND,
        TXPOOL_DEFAULT_NONCE_BITS,
        machine.clone(),
    );

    let txpool = Arc::new(TransactionPool::new(
        TxPoolConfig::default(),
        verification_config.clone(),
        data_man.clone(),
        machine.clone(),
    ));
    let statistics = Arc::new(Statistics::new());

    let pow_config = ProofOfWorkConfig::new(
        true,      /* test_mode */
        "disable", /* mining_type */
        Some(10),
        String::from(""), /* stratum_listen_addr */
        0,                /* stratum_port */
        None,             /* stratum_secret */
        1,                /* pow_problem_window_size */
    );
    let sync_config = SyncGraphConfig {
        future_block_buffer_capacity: 1,
        enable_state_expose: false,
        is_consortium: false,
    };
    let notifications = Notifications::init();
    let consensus = Arc::new(ConsensusGraph::new(
        ConsensusConfig {
            chain_id: ChainIdParamsInner::new_simple(AllChainID::new(1, 1)),
            transaction_epoch_bound: TRANSACTION_DEFAULT_EPOCH_BOUND,
            referee_bound: REFEREE_DEFAULT_BOUND,
            get_logs_epoch_batch_size: 32,
            get_logs_filter_max_epoch_range: None,
            get_logs_filter_max_block_number_range: None,
            get_logs_filter_max_limit: None,
            sync_state_starting_epoch: None,
            sync_state_epoch_gap: None,
        },
        txpool.clone(),
        statistics.clone(),
        data_man.clone(),
        pow_config.clone(),
        pow.clone(),
        notifications.clone(),
        ConsensusExecutionConfiguration {
            executive_trace: false,
        },
        verification_config.clone(),
        NodeType::Archive,
    ));

    let sync = Arc::new(SynchronizationGraph::new(
        consensus.clone(),
        verification_config,
        pow_config,
        pow.clone(),
        sync_config,
        notifications,
        machine,
    ));

    (sync, consensus)
}

/// This method is only used in tests and benchmarks.
pub fn initialize_synchronization_graph(
    db_dir: &str, beta: u64, h: u64, tcr: u64, tcb: u64, era_epoch_count: u64,
    dbtype: DbType, seed_hash: H256,
) -> (
    Arc<SynchronizationGraph>,
    Arc<ConsensusGraph>,
    Arc<BlockDataManager>,
    Arc<Block>,
) {
    let vm = VmFactory::new(1024 * 32);
    let pow = Arc::new(PowComputer::new(seed_hash));

    let (data_man, genesis_block) =
        initialize_data_manager(db_dir, dbtype, pow.clone(), vm.clone());

    let (sync, consensus) = initialize_synchronization_graph_with_data_manager(
        data_man.clone(),
        beta,
        h,
        tcr,
        tcb,
        era_epoch_count,
        pow,
        vm,
    );

    (sync, consensus, data_man, genesis_block)
}
