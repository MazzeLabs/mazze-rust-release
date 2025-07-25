// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/
pub mod miner;

use crate::miner::{
    stratum::{Options as StratumOption, Stratum},
    work_notify::NotifyWork,
};
use lazy_static::lazy_static;
use log::{debug, info, trace, warn};
use mazze_parameters::{
    consensus::GENESIS_GAS_LIMIT, consensus_internal::ELASTICITY_MULTIPLIER,
};
use mazze_types::{Address, SpaceMap, H256, U256};
use mazzecore::{
    block_parameters::*, consensus::consensus_inner::StateBlameInfo, pow::*,
    verification::compute_transaction_root, ConsensusGraph,
    ConsensusGraphTrait, SharedSynchronizationGraph,
    SharedSynchronizationService, SharedTransactionPool, Stopable,
};
use metrics::{Gauge, GaugeUsize};
use parking_lot::{Mutex, RwLock};
use primitives::*;
use std::{
    cmp::max,
    collections::HashSet,
    sync::{
        mpsc::{self, TryRecvError},
        Arc,
    },
    thread,
    time::{self, Instant},
};
use time::{Duration, SystemTime, UNIX_EPOCH};
use txgen::SharedTransactionGenerator;
lazy_static! {
    static ref PACKED_ACCOUNT_SIZE: Arc<dyn Gauge<usize>> =
        GaugeUsize::register_with_group("txpool", "packed_account_size");
}

/// This determined the frequency of checking a new PoW problem.
/// And the current mining speed in the Rust implementation is about 2 ms per
/// nonce.
const MINING_ITERATION: u64 = 20;
const BLOCK_FORCE_UPDATE_INTERVAL_IN_SECS: u64 = 10;
const BLOCKGEN_LOOP_SLEEP_IN_MILISECS: u64 = 30;

enum MiningState {
    Start,
    Stop,
}

/// The interface for a mazze block generator
pub struct BlockGenerator {
    pub pow_config: ProofOfWorkConfig,
    pow: Arc<PowComputer>,
    mining_author: Address,
    graph: SharedSynchronizationGraph,
    txpool: SharedTransactionPool,
    maybe_txgen: Option<SharedTransactionGenerator>,
    sync: SharedSynchronizationService,
    state: RwLock<MiningState>,
    workers: Mutex<Vec<(Worker, mpsc::Sender<ProofOfWorkProblem>)>>,
    pub stratum: RwLock<Option<Stratum>>,
}

pub struct Worker {
    #[allow(dead_code)]
    thread: thread::JoinHandle<()>,
}

impl Worker {
    pub fn new(
        bg: Arc<BlockGenerator>,
        solution_sender: mpsc::Sender<ProofOfWorkSolution>,
        problem_receiver: mpsc::Receiver<ProofOfWorkProblem>, seed_hash: H256,
    ) -> Self {
        let bg_handle = bg;

        let thread = thread::Builder::new()
            .name("blockgen".into())
            .spawn(move || {
                let sleep_duration = time::Duration::from_millis(100);
                let mut problem: Option<ProofOfWorkProblem> = None;
                let bg_pow = Arc::new(PowComputer::new(seed_hash));

                loop {
                    match *bg_handle.state.read() {
                        MiningState::Stop => return,
                        _ => {}
                    }

                    // Drain the channel to check the latest problem
                    loop {
                        let maybe_new_problem = problem_receiver.try_recv();
                        trace!("new problem: {:?}", problem);
                        match maybe_new_problem {
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => return,
                            Ok(new_problem) => {
                                problem = Some(new_problem);
                            }
                        }
                    }
                    // check if there is a problem to be solved
                    if problem.is_some() {
                        trace!("problem is {:?}", problem);
                        let boundary = problem.as_ref().unwrap().boundary;
                        let block_hash = problem.as_ref().unwrap().block_hash;
                        let seed_hash = problem.as_ref().unwrap().seed_hash;
                        let mut nonce: u64 = rand::random();
                        for _i in 0..MINING_ITERATION {
                            let nonce_u256 = U256::from(nonce);
                            let hash = bg_pow.compute(&nonce_u256, &block_hash, &seed_hash);
                            if ProofOfWorkProblem::validate_hash_against_boundary(&hash, &nonce_u256, &boundary) {
                                // problem solved
                                match solution_sender
                                    .send(ProofOfWorkSolution { nonce: nonce_u256 })
                                {
                                    Ok(_) => {}
                                    Err(e) => {
                                        warn!("{}", e);
                                    }
                                }

                                trace!("problem solved");
                                problem = None;
                                break;
                            }
                            nonce += 1;
                        }
                    } else {
                        thread::sleep(sleep_duration);
                    }
                }
            })
            .expect("only one blockgen thread, so it should not fail");
        Worker { thread }
    }
}

impl BlockGenerator {
    pub fn new(
        graph: SharedSynchronizationGraph, txpool: SharedTransactionPool,
        sync: SharedSynchronizationService,
        maybe_txgen: Option<SharedTransactionGenerator>,
        pow_config: ProofOfWorkConfig, pow: Arc<PowComputer>,
        mining_author: Address,
    ) -> Self {
        info!(
            "Initial mining difficulty set to: {:?}",
            pow_config.initial_difficulty
        );
        BlockGenerator {
            pow_config,
            pow,
            mining_author,
            graph,
            txpool,
            maybe_txgen,
            sync,
            state: RwLock::new(MiningState::Start),
            workers: Mutex::new(Vec::new()),
            stratum: RwLock::new(None),
        }
    }

    fn consensus_graph(&self) -> &ConsensusGraph {
        self.graph
            .consensus
            .as_any()
            .downcast_ref::<ConsensusGraph>()
            .expect("downcast should succeed")
    }

    /// Stop mining
    pub fn stop(&self) {
        {
            let mut write = self.state.write();
            *write = MiningState::Stop;
        }
        if let Some(txgen) = self.maybe_txgen.as_ref() {
            txgen.stop()
        }
    }

    /// Send new PoW problem to workers
    pub fn send_problem(bg: Arc<BlockGenerator>, problem: ProofOfWorkProblem) {
        match bg.pow_config.mining_type {
            MiningType::Stratum => {
                let stratum = bg.stratum.read();
                stratum.as_ref().unwrap().notify(problem);
            }
            MiningType::CPU => {
                for item in bg.workers.lock().iter() {
                    item.1
                        .send(problem)
                        .expect("Failed to send the PoW problem.")
                }
            }
            MiningType::Disable => {
                // No action needed for disabled mining
                debug!("Mining is disabled. Ignoring PoW problem.");
            }
        }
    }

    // TODO: should not hold and pass write lock to consensus.
    fn assemble_new_block_impl(
        &self, mut parent_hash: H256, mut referees: Vec<H256>,
        mut blame_info: StateBlameInfo, block_gas_limit: U256,
        transactions: Vec<Arc<SignedTransaction>>, difficulty: u64,
        adaptive_opt: Option<bool>, maybe_base_price: Option<SpaceMap<U256>>,
    ) -> Block {
        trace!("{} txs packed", transactions.len());
        let consensus_graph = self.consensus_graph();
        if adaptive_opt.is_none() {
            // This is the normal case for mining.
            consensus_graph.choose_correct_parent(
                &mut parent_hash,
                &mut referees,
                &mut blame_info,
            );
        }
        let mut consensus_inner = consensus_graph.inner.write();
        // referees are retrieved before locking inner, so we need to
        // filter out the blocks that should be removed by possible
        // checkpoint making that happens before we acquire the inner lock
        referees
            .retain(|h| consensus_inner.hash_to_arena_indices.contains_key(h));
        let mut expected_difficulty =
            consensus_inner.expected_difficulty(&parent_hash);
        let adaptive = if let Some(x) = adaptive_opt {
            x
        } else {
            consensus_graph.check_mining_adaptive_block(
                &mut *consensus_inner,
                &parent_hash,
                &referees,
                &expected_difficulty,
            )
        };

        let (parent_height, parent_timestamp) = {
            let parent_header = consensus_inner
                .data_man
                .block_header_by_hash(&parent_hash)
                .unwrap();
            (parent_header.height(), parent_header.timestamp())
        };

        if U256::from(difficulty) > expected_difficulty {
            expected_difficulty = U256::from(difficulty);
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Adjust the timestamp of the currently mined block to be later
        // than or equal to its parent's.
        // See comments in verify_header_graph_ready_block()
        let my_timestamp = max(parent_timestamp, now);

        let custom = self
            .txpool
            .machine()
            .params()
            .custom_prefix(parent_height + 1)
            .unwrap_or(vec![]);
        let block_header = BlockHeaderBuilder::new()
            .with_transactions_root(compute_transaction_root(&transactions))
            .with_parent_hash(parent_hash)
            .with_height(parent_height + 1)
            .with_timestamp(my_timestamp)
            .with_author(self.mining_author)
            .with_blame(blame_info.blame)
            .with_deferred_state_root(blame_info.state_vec_root)
            .with_deferred_receipts_root(blame_info.receipts_vec_root)
            .with_deferred_logs_bloom_hash(blame_info.logs_bloom_vec_root)
            .with_difficulty(expected_difficulty)
            .with_adaptive(adaptive)
            .with_referee_hashes(referees)
            .with_nonce(U256::zero())
            .with_gas_limit(block_gas_limit)
            .with_custom(custom)
            .with_base_price(maybe_base_price)
            .build();

        Block::new(block_header, transactions)
    }

    /// Assemble a new block with specified parent and referee, this is for test
    /// only
    pub fn assemble_new_fixed_block(
        &self, parent_hash: H256, referee: Vec<H256>, num_txs: usize,
        difficulty: u64, adaptive: bool, block_gas_target: u64,
    ) -> Result<Block, String> {
        let consensus_graph = self.consensus_graph();
        let state_blame_info = consensus_graph
            .force_compute_blame_and_deferred_state_for_generation(
                &parent_hash,
            )?;

        let block_size_limit =
            self.graph.verification_config.max_block_size_in_bytes;
        let best_info = consensus_graph.best_info();

        let parent_block = self
            .txpool
            .data_man
            .block_header_by_hash(&best_info.best_block_hash)
            // The parent block must exists.
            .expect("Parent block not found");

        let machine = self.txpool.machine();
        let params = machine.params();

        let pack_height = best_info.best_epoch_number + 1;

        let block_gas_limit =
            (block_gas_target * ELASTICITY_MULTIPLIER as u64).into();

        let parent_base_price = if 1 == pack_height {
            params.init_base_price()
        } else {
            parent_block.base_price().unwrap()
        };

        let (transactions, maybe_base_price) =
            self.txpool.pack_transactions_1559(
                num_txs,
                block_gas_limit,
                parent_base_price,
                block_size_limit,
                best_info.best_epoch_number,
                best_info.best_block_number,
            );

        Ok(self.assemble_new_block_impl(
            parent_hash,
            referee,
            state_blame_info,
            block_gas_limit,
            transactions,
            difficulty,
            Some(adaptive),
            Some(maybe_base_price),
        ))
    }

    /// Assemble a new block without nonce
    pub fn assemble_new_block(
        &self, num_txs: usize, block_size_limit: usize,
        additional_transactions: Vec<Arc<SignedTransaction>>,
    ) -> Block {
        let consensus_graph = self.consensus_graph();

        let (best_info, block_gas_limit, transactions, maybe_base_price) =
            self.txpool.get_best_info_with_packed_transactions(
                num_txs,
                block_size_limit,
                additional_transactions,
            );

        let mut sender_accounts = HashSet::new();
        for tx in &transactions {
            let tx_hash = tx.hash();
            if tx_hash[0] & 254 == 0 {
                debug!("Sampled transaction {:?} in packing block", tx_hash);
            }
            sender_accounts.insert(tx.sender);
        }
        PACKED_ACCOUNT_SIZE.update(sender_accounts.len());

        let state_blame_info = match consensus_graph
            .get_blame_and_deferred_state_for_generation(
                &best_info.best_block_hash,
            ) {
            Ok(info) => info,
            Err(e) => {
                warn!("Cannot get blame and deferred state for generation: {}. Using default state blame info.", e);
                // Return a default state blame info
                StateBlameInfo {
                    blame: 0,
                    state_vec_root: H256::default(),
                    receipts_vec_root: H256::default(),
                    logs_bloom_vec_root: H256::default(),
                }
            }
        };

        let best_block_hash = best_info.best_block_hash.clone();
        let mut referee = best_info.bounded_terminal_block_hashes.clone();

        referee.retain(|r| *r != best_block_hash);

        self.assemble_new_block_impl(
            best_block_hash,
            referee,
            state_blame_info,
            block_gas_limit,
            transactions,
            0,
            None,
            maybe_base_price,
        )
    }

    /// Assemble a new block without nonce and with options to override the
    /// states/blame. This function is used for testing only to generate
    /// incorrect blocks
    pub fn assemble_new_block_with_blame_info(
        &self, num_txs: usize, block_size_limit: usize,
        additional_transactions: Vec<Arc<SignedTransaction>>,
        blame_override: Option<u32>, state_root_override: Option<H256>,
        receipt_root_override: Option<H256>,
        logs_bloom_hash_override: Option<H256>,
    ) -> Block {
        let consensus_graph = self.consensus_graph();

        let (best_info, block_gas_limit, transactions, maybe_base_price) =
            self.txpool.get_best_info_with_packed_transactions(
                num_txs,
                block_size_limit,
                additional_transactions,
            );

        let mut state_blame_info = consensus_graph
            .get_blame_and_deferred_state_for_generation(
                &best_info.best_block_hash,
            )
            .unwrap();

        if let Some(x) = blame_override {
            state_blame_info.blame = x;
        }
        if let Some(x) = state_root_override {
            state_blame_info.state_vec_root = x;
        }
        if let Some(x) = receipt_root_override {
            state_blame_info.receipts_vec_root = x;
        }
        if let Some(x) = logs_bloom_hash_override {
            state_blame_info.logs_bloom_vec_root = x;
        }

        let best_block_hash = best_info.best_block_hash.clone();
        let mut referee = best_info.bounded_terminal_block_hashes.clone();
        referee.retain(|r| *r != best_block_hash);

        self.assemble_new_block_impl(
            best_block_hash,
            referee,
            state_blame_info,
            block_gas_limit,
            transactions,
            0,
            None,
            maybe_base_price,
        )
    }

    /// Update and sync a new block
    pub fn on_mined_block(&self, block: Block) {
        // FIXME: error handling.
        self.sync.on_mined_block(block).ok();
    }

    /// Check if we need to mine on a new block
    pub fn is_mining_block_outdated(
        &self, block: Option<&Block>, last_assemble: &SystemTime,
    ) -> bool {
        if block.is_none() {
            return true;
        }

        // 1st Check: if the parent block changed
        let best_block_hash = self.graph.consensus.best_block_hash();
        if best_block_hash != *block.unwrap().block_header.parent_hash() {
            return true;
        }

        // 2nd Check: if the last block is too old, we will generate a new
        // block. Checking transaction updates and referees might be
        // costly and the trade-off is unclear here. It is simple to
        // just enforce a time here.
        let elapsed = last_assemble.elapsed();
        if let Ok(d) = elapsed {
            if d > Duration::from_secs(BLOCK_FORCE_UPDATE_INTERVAL_IN_SECS) {
                return true;
            }
        }

        false
    }

    // This function is used in test only to simulate attacker behavior.
    pub fn generate_fixed_block(
        &self, parent_hash: H256, referee: Vec<H256>, num_txs: usize,
        difficulty: u64, adaptive: bool,
    ) -> Result<H256, String> {
        let block = self.assemble_new_fixed_block(
            parent_hash,
            referee,
            num_txs,
            difficulty,
            adaptive,
            GENESIS_GAS_LIMIT,
        )?;
        Ok(self.generate_block_impl(block))
    }

    /// Generate a block with transactions in the pool
    pub fn generate_block(
        &self, num_txs: usize, block_size_limit: usize,
        additional_transactions: Vec<Arc<SignedTransaction>>,
    ) -> H256 {
        let block = self.assemble_new_block(
            num_txs,
            block_size_limit,
            additional_transactions,
        );
        self.generate_block_impl(block)
    }

    /// Generate a block with transactions in the pool.
    /// This is used for testing only
    pub fn generate_block_with_blame_info(
        &self, num_txs: usize, block_size_limit: usize,
        additional_transactions: Vec<Arc<SignedTransaction>>,
        blame: Option<u32>, state_root: Option<H256>,
        receipts_root: Option<H256>, logs_bloom_hash: Option<H256>,
    ) -> H256 {
        let block = self.assemble_new_block_with_blame_info(
            num_txs,
            block_size_limit,
            additional_transactions,
            blame,
            state_root,
            receipts_root,
            logs_bloom_hash,
        );
        self.generate_block_impl(block)
    }

    pub fn generate_custom_block(
        &self, transactions: Vec<Arc<SignedTransaction>>,
        adaptive: Option<bool>,
    ) -> H256 {
        let consensus_graph = self.consensus_graph();
        // get the best block
        let (best_info, _, _, _) = self
            .txpool
            .get_best_info_with_packed_transactions(0, 0, Vec::new());

        let parent_hash = best_info.best_block_hash;
        let maybe_base_price = self
            .txpool
            .compute_1559_base_price(
                &parent_hash,
                (GENESIS_GAS_LIMIT * ELASTICITY_MULTIPLIER as u64).into(),
                transactions.iter().map(|x| &**x),
            )
            .unwrap();
        let block_gas_limit = GENESIS_GAS_LIMIT
            * if maybe_base_price.is_some() {
                ELASTICITY_MULTIPLIER as u64
            } else {
                1
            };

        let state_blame_info = consensus_graph
            .get_blame_and_deferred_state_for_generation(
                &best_info.best_block_hash,
            )
            .unwrap();

        let best_block_hash = best_info.best_block_hash.clone();
        let mut referee = best_info.bounded_terminal_block_hashes.clone();
        referee.retain(|r| *r != best_block_hash);

        let block = self.assemble_new_block_impl(
            best_block_hash,
            referee,
            state_blame_info,
            block_gas_limit.into(),
            transactions,
            0,
            adaptive,
            maybe_base_price,
        );

        self.generate_block_impl(block)
    }

    pub fn generate_custom_block_with_parent(
        &self, parent_hash: H256, referee: Vec<H256>,
        transactions: Vec<Arc<SignedTransaction>>, adaptive: bool,
        maybe_custom: Option<Vec<Vec<u8>>>,
    ) -> Result<H256, String> {
        let consensus_graph = self.consensus_graph();
        let state_blame_info = consensus_graph
            .force_compute_blame_and_deferred_state_for_generation(
                &parent_hash,
            )?;

        let maybe_base_price = self
            .txpool
            .compute_1559_base_price(
                &parent_hash,
                (GENESIS_GAS_LIMIT * ELASTICITY_MULTIPLIER as u64).into(),
                transactions.iter().map(|x| &**x),
            )
            .expect("Cannot compute base price");

        let block_gas_limit = if maybe_base_price.is_some() {
            GENESIS_GAS_LIMIT * ELASTICITY_MULTIPLIER as u64
        } else {
            GENESIS_GAS_LIMIT
        };

        let mut block = self.assemble_new_block_impl(
            parent_hash,
            referee,
            state_blame_info,
            block_gas_limit.into(),
            transactions,
            0,
            Some(adaptive),
            maybe_base_price,
        );
        if let Some(custom) = maybe_custom {
            block.block_header.set_custom(custom);
        }

        Ok(self.generate_block_impl(block))
    }

    pub fn generate_block_with_nonce_and_timestamp(
        &self, parent_hash: H256, referee: Vec<H256>,
        transactions: Vec<Arc<SignedTransaction>>, nonce: U256, timestamp: u64,
        adaptive: bool,
    ) -> Result<H256, String> {
        let consensus_graph = self.consensus_graph();
        let state_blame_info = consensus_graph
            .force_compute_blame_and_deferred_state_for_generation(
                &parent_hash,
            )?;

        let maybe_base_price = self
            .txpool
            .compute_1559_base_price(
                &parent_hash,
                (GENESIS_GAS_LIMIT * ELASTICITY_MULTIPLIER as u64).into(),
                transactions.iter().map(|x| &**x),
            )
            .expect("Cannot compute base price");

        let block_gas_limit = if maybe_base_price.is_some() {
            GENESIS_GAS_LIMIT * ELASTICITY_MULTIPLIER as u64
        } else {
            GENESIS_GAS_LIMIT
        };

        let mut block = self.assemble_new_block_impl(
            parent_hash,
            referee,
            state_blame_info,
            block_gas_limit.into(),
            transactions,
            0,
            Some(adaptive),
            maybe_base_price,
        );
        block.block_header.set_nonce(nonce);
        block.block_header.set_timestamp(timestamp);

        let hash = block.block_header.compute_hash();
        debug!(
            "generate_block with block header:{:?} tx_number:{}, block_size:{}",
            block.block_header,
            block.transactions.len(),
            block.size(),
        );
        self.on_mined_block(block);

        consensus_graph.wait_for_generation(&hash);
        Ok(hash)
    }

    fn generate_block_impl(&self, block_init: Block) -> H256 {
        let mut block = block_init;
        let difficulty = block.block_header.difficulty();
        let problem = ProofOfWorkProblem::new(
            block.block_header.height(),
            block.block_header.problem_hash(),
            *difficulty,
            self.consensus_graph()
                .get_data_manager()
                .db_manager
                .get_current_seed_hash(block.block_header.height()),
        );
        let mut nonce: u64 = rand::random();
        loop {
            if validate(
                self.pow.clone(),
                &problem,
                &ProofOfWorkSolution {
                    nonce: U256::from(nonce),
                },
            ) {
                block.block_header.set_nonce(U256::from(nonce));
                break;
            }
            nonce += 1;
        }
        let hash = block.block_header.compute_hash();
        debug!(
            "generate_block with block header:{:?} tx_number:{}, block_size:{}",
            block.block_header,
            block.transactions.len(),
            block.size(),
        );
        self.on_mined_block(block);

        debug!("generate_block finished on_mined_block()");
        // FIXME: We should add a flag to enable/disable this wait
        // Ensure that when `generate**` function returns, the block has been
        // handled by Consensus This order is assumed by some tests, and
        // this function is also only used in tests.
        self.consensus_graph().wait_for_generation(&hash);
        debug!("generate_block finished wait_for_generation()");

        hash
    }

    pub fn pow_config(&self) -> ProofOfWorkConfig {
        self.pow_config.clone()
    }

    /// Start num_worker new workers
    pub fn start_new_worker(
        num_worker: u32, bg: Arc<BlockGenerator>, seed_hash: H256,
    ) -> mpsc::Receiver<ProofOfWorkSolution> {
        let (solution_sender, solution_receiver) = mpsc::channel();
        let mut workers = bg.workers.lock();
        for _ in 0..num_worker {
            let (problem_sender, problem_receiver) = mpsc::channel();
            workers.push((
                Worker::new(
                    bg.clone(),
                    solution_sender.clone(),
                    problem_receiver,
                    seed_hash,
                ),
                problem_sender,
            ));
        }
        solution_receiver
    }

    pub fn start_new_stratum_worker(
        bg: Arc<BlockGenerator>,
    ) -> mpsc::Receiver<ProofOfWorkSolution> {
        let (solution_sender, solution_receiver) = mpsc::channel();
        let cfg = StratumOption {
            listen_addr: bg.pow_config.stratum_listen_addr.clone(),
            port: bg.pow_config.stratum_port,
            secret: bg.pow_config.stratum_secret,
        };
        let stratum = Stratum::start(
            &cfg,
            bg.pow.clone(),
            bg.pow_config.pow_problem_window_size,
            solution_sender,
        )
        .expect("Failed to start Stratum service.");
        let mut bg_stratum = bg.stratum.write();
        *bg_stratum = Some(stratum);
        solution_receiver
    }

    pub fn start_mining(
        bg: Arc<BlockGenerator>, _payload_len: u32, seed_hash: H256,
    ) {
        match bg.pow_config.mining_type {
            MiningType::Disable => {
                debug!("Mining is disabled.");
                return;
            }
            MiningType::CPU => {
                debug!("Starting CPU mining.");
                BlockGenerator::start_cpu_mining(bg, _payload_len, seed_hash);
            }
            MiningType::Stratum => {
                debug!("Starting Stratum mining server.");
                BlockGenerator::start_stratum_mining(bg, _payload_len);
            }
        }
    }

    fn start_cpu_mining(
        bg: Arc<BlockGenerator>, _payload_len: u32, seed_hash: H256,
    ) {
        let pow_computer = Arc::new(PowComputer::new(seed_hash));
        let mut current_mining_block: Option<Block> = None;
        let mut current_problem: Option<ProofOfWorkProblem> = None;
        let mut last_assemble = SystemTime::now();
        let sleep_duration = Duration::from_millis(100);

        loop {
            match *bg.state.read() {
                MiningState::Stop => return,
                _ => {}
            }

            if bg.is_mining_block_outdated(
                current_mining_block.as_ref(),
                &last_assemble,
            ) {
                // Try to assemble new block with panic handling
                let new_block = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    bg.assemble_new_block(
                        MAX_TRANSACTION_COUNT_PER_BLOCK,
                        bg.graph.verification_config.max_block_size_in_bytes,
                        vec![],
                    )
                })) {
                    Ok(block) => block,
                    Err(_) => {
                        warn!("Failed to assemble new block, execution results not ready. Retrying...");
                        thread::sleep(sleep_duration);
                        continue;
                    }
                };

                let problem = ProofOfWorkProblem::new(
                    new_block.block_header.height(),
                    new_block.block_header.problem_hash(),
                    *new_block.block_header.difficulty(),
                    bg.consensus_graph()
                        .get_data_manager()
                        .db_manager
                        .get_current_seed_hash(new_block.block_header.height()),
                );

                current_mining_block = Some(new_block);
                current_problem = Some(problem);
                last_assemble = SystemTime::now();
            }

            if let Some(problem) = current_problem.as_ref() {
                match Self::get_problem_solution(
                    &pow_computer,
                    problem,
                    Duration::from_secs(5),
                ) {
                    Some(solution) => {
                        info!(
                            "Found mining solution: nonce = {:?}",
                            solution.nonce
                        );
                        if let Some(mut block) = current_mining_block.take() {
                            block.block_header.set_nonce(solution.nonce);
                            let hash = block.block_header.compute_hash();
                            bg.on_mined_block(block);
                            info!("Mined block with hash: {:?}", hash);
                        }
                        current_problem = None;
                    }
                    None => {
                        info!(
                            "No solution found in this attempt, continuing..."
                        );
                    }
                }
            }

            thread::sleep(Duration::from_millis(10));
        }
    }

    fn get_problem_solution(
        pow_computer: &PowComputer, problem: &ProofOfWorkProblem,
        timeout: Duration,
    ) -> Option<ProofOfWorkSolution> {
        info!(
            "Starting mining attempt with difficulty: {:?}",
            problem.difficulty
        );
        let start_time = Instant::now();
        let mut nonce = U256::zero();
        let mut hashes_checked = 0;

        while start_time.elapsed() < timeout {
            let hash = pow_computer.compute(
                &nonce,
                &problem.block_hash,
                &problem.seed_hash,
            );
            hashes_checked += 1;

            if ProofOfWorkProblem::validate_hash_against_boundary(
                &hash,
                &nonce,
                &problem.boundary,
            ) {
                info!(
                    "Solution found after checking {} hashes",
                    hashes_checked
                );
                return Some(ProofOfWorkSolution { nonce });
            }

            nonce = nonce.overflowing_add(U256::one()).0;
        }

        None
    }

    fn start_stratum_mining(bg: Arc<BlockGenerator>, _payload_len: u32) {
        let mut current_mining_block = None;
        let mut recent_mining_blocks = vec![];
        let mut recent_mining_problems = vec![];
        let mut current_problem: Option<ProofOfWorkProblem> = None;
        let sleep_duration =
            time::Duration::from_millis(BLOCKGEN_LOOP_SLEEP_IN_MILISECS);

        let receiver: mpsc::Receiver<ProofOfWorkSolution> =
            BlockGenerator::start_new_stratum_worker(bg.clone());

        let mut last_notify = SystemTime::now();
        let mut last_assemble = SystemTime::now();
        loop {
            match *bg.state.read() {
                MiningState::Stop => return,
                _ => {}
            }

            if bg.is_mining_block_outdated(
                current_mining_block.as_ref(),
                &last_assemble,
            ) {
                // TODO: #transations TBD
                if !bg.pow_config.test_mode && bg.sync.catch_up_mode() {
                    thread::sleep(sleep_duration);
                    continue;
                }

                current_mining_block = Some(bg.assemble_new_block(
                    MAX_TRANSACTION_COUNT_PER_BLOCK,
                    bg.graph.verification_config.max_block_size_in_bytes,
                    vec![],
                ));

                if recent_mining_blocks.len()
                    == bg.pow_config.pow_problem_window_size
                {
                    recent_mining_blocks.remove(0);
                    recent_mining_problems.remove(0);
                }

                // set a mining problem
                let current_difficulty = current_mining_block
                    .as_ref()
                    .unwrap()
                    .block_header
                    .difficulty();
                debug!("Current difficulty: {:?}", current_difficulty);
                let height = current_mining_block
                    .as_ref()
                    .unwrap()
                    .block_header
                    .height();
                let problem = ProofOfWorkProblem::new(
                    height,
                    current_mining_block
                        .as_ref()
                        .unwrap()
                        .block_header
                        .problem_hash(),
                    *current_difficulty,
                    bg.consensus_graph()
                        .get_data_manager()
                        .db_manager
                        .get_current_seed_hash(height),
                );
                last_assemble = SystemTime::now();
                trace!("send problem: {:?}", problem);
                BlockGenerator::send_problem(bg.clone(), problem);
                last_notify = SystemTime::now();
                current_problem = Some(problem);

                recent_mining_blocks
                    .push(current_mining_block.clone().unwrap());
                recent_mining_problems.push(problem);
            } else {
                // check if the problem solved
                let mut new_solution = receiver.try_recv();
                let mut maybe_mined_block = None;

                loop {
                    trace!("new solution: {:?}", new_solution);
                    // check if the block received valid
                    if !new_solution.is_ok() {
                        break;
                    }

                    for index in 0..recent_mining_problems.len() {
                        if validate(
                            bg.pow.clone(),
                            &recent_mining_problems[index],
                            &new_solution.unwrap(),
                        ) {
                            maybe_mined_block =
                                Some(recent_mining_blocks[index].clone());
                            break;
                        }
                    }

                    if maybe_mined_block.is_none() {
                        warn!(
                            "Received invalid solution from miner: nonce = {}!",
                            &new_solution.unwrap().nonce
                        );
                        new_solution = receiver.try_recv();
                        continue;
                    }
                    break;
                }

                if new_solution.is_ok() {
                    let solution = new_solution.unwrap();
                    let mut mined_block = maybe_mined_block.unwrap();
                    mined_block.block_header.set_nonce(solution.nonce);
                    mined_block.block_header.compute_hash();
                    bg.on_mined_block(mined_block);
                    current_mining_block = None;
                    current_problem = None;
                } else {
                    // We will send out heartbeat because newcomers or
                    // disconnected people may lose the previous message
                    if let Some(problem) = current_problem {
                        if let Ok(elapsed) = last_notify.elapsed() {
                            if bg.pow_config.use_stratum()
                                && elapsed > Duration::from_secs(60)
                            {
                                BlockGenerator::send_problem(
                                    bg.clone(),
                                    problem,
                                );
                                last_notify = SystemTime::now();
                            }
                        } else {
                            warn!("Unable to get system time. Stratum heartbeat message canceled!")
                        }
                    }
                    // wait a moment and check again
                    thread::sleep(sleep_duration);
                    continue;
                }
            }
        }
    }

    pub fn auto_block_generation(&self, interval_ms: u64) {
        let interval = Duration::from_millis(interval_ms);
        loop {
            match *self.state.read() {
                MiningState::Stop => return,
                _ => {}
            }
            if !self.sync.catch_up_mode() {
                self.generate_block(
                    3000,
                    self.graph.verification_config.max_block_size_in_bytes,
                    vec![],
                );
            }
            thread::sleep(interval);
        }
    }
}

impl Stopable for BlockGenerator {
    fn stop(&self) {
        Self::stop(self)
    }
}
