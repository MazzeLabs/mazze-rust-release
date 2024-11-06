use core_affinity;
use log::{info, trace, warn};
use mazze_types::{H256, U256};
use mazzecore::pow::{
    boundary_to_difficulty, ProofOfWorkProblem, ProofOfWorkSolution,
};
use randomx_rs::{RandomXCache, RandomXFlag, RandomXVM};
use serde_json::Value;
use std::arch::x86_64::{
    __m256i, _mm256_cmpgt_epi8, _mm256_loadu_si256, _mm256_set1_epi8,
    _mm256_testc_si256,
};
use std::str::FromStr;
use std::sync::atomic::{self, AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

const CYCLE_LENGTH: u64 = 1200; // 1 second
                                // TODO: make this adjustable based on difficulty
const CHECK_INTERVAL: u64 = 2; // Check for new problem every 2 nonces

const BATCH_SIZE: usize = 8;

struct BatchHasher {
    inputs: Vec<Vec<u8>>,
    prefetch_buffer: Vec<u8>, // New field
}

impl BatchHasher {
    fn new() -> Self {
        Self {
            inputs: Vec::with_capacity(BATCH_SIZE),
            prefetch_buffer: vec![0u8; 64 * BATCH_SIZE * 2],
        }
    }

    fn prepare_batch(&mut self, start_nonce: U256, block_hash: &H256) {
        self.inputs.clear();

        for i in 0..BATCH_SIZE {
            let nonce = start_nonce + i;
            let mut input = vec![0u8; 64];

            // Copy block hash
            input[..32].copy_from_slice(block_hash.as_bytes());

            // Set nonce
            nonce.to_little_endian(&mut input[32..64]);

            self.inputs.push(input);
        }
    }

    fn compute_hash_batch(
        &mut self, vm: &RandomXVM, start_nonce: U256, block_hash: &H256,
    ) -> Vec<H256> {
        self.prepare_batch(start_nonce, block_hash);

        let input_refs: Vec<&[u8]> =
            self.inputs.iter().map(|v| v.as_slice()).collect();

        let hashes = vm
            .calculate_hash_set(&input_refs)
            .expect("Failed to calculate hash batch");

        hashes
            .into_iter()
            .map(|hash| H256::from_slice(&hash))
            .collect()
    }

    #[cfg(target_arch = "x86_64")]
    unsafe fn compare_hashes_simd(
        &self, hashes: &[H256], boundary: &U256,
    ) -> Option<usize> {
        let mut boundary_bytes = [0u8; 32];
        boundary.to_little_endian(&mut boundary_bytes);
        let boundary_vec =
            _mm256_loadu_si256(boundary_bytes.as_ptr() as *const __m256i);

        for (i, hash) in hashes.iter().enumerate() {
            let hash_vec =
                _mm256_loadu_si256(hash.as_bytes().as_ptr() as *const __m256i);
            let cmp = _mm256_cmpgt_epi8(boundary_vec, hash_vec);
            if _mm256_testc_si256(cmp, _mm256_set1_epi8(-1)) != 0 {
                return Some(i);
            }
        }
        None
    }
}

struct LocalProblemState {
    problem: ProofOfWorkProblem,
    hash_chunks: [u64; 4],
}

impl LocalProblemState {
    fn new(problem: ProofOfWorkProblem) -> Self {
        let hash_bytes = problem.block_hash.as_bytes();
        let mut hash_chunks = [0u64; 4];

        for i in 0..4 {
            hash_chunks[i] = u64::from_le_bytes(
                hash_bytes[i * 8..(i + 1) * 8].try_into().unwrap(),
            );
        }

        Self {
            problem,
            hash_chunks,
        }
    }
}
struct AtomicProblemState {
    block_height: AtomicU64,
    block_hash: [AtomicU64; 4], // H256 split into 4 u64s for atomic access
    is_active: AtomicBool,
}

struct MiningState {
    current_problem: Option<ProofOfWorkProblem>,
}

#[derive(Clone)]
pub struct Miner {
    pub worker_id: usize,
    pub worker_name: String,
    num_threads: usize,
    state: Arc<RwLock<MiningState>>,
    atomic_state: Arc<AtomicProblemState>,
    solution_sender: mpsc::Sender<(ProofOfWorkSolution, ProofOfWorkProblem)>,
}

struct ThreadLocalVM {
    vm: RandomXVM,
    cache: RandomXCache,
    current_block_hash: H256,
    flags: RandomXFlag,
}

impl ThreadLocalVM {
    fn new(flags: RandomXFlag, block_hash: &H256) -> Self {
        let cache = RandomXCache::new(flags, block_hash.as_bytes())
            .expect("Failed to create RandomX cache");
        let vm = RandomXVM::new(flags, Some(cache.clone()), None)
            .expect("Failed to create RandomX VM");

        Self {
            vm,
            cache,
            current_block_hash: *block_hash,
            flags,
        }
    }

    fn update_if_needed(&mut self, block_hash: &H256) {
        if self.current_block_hash != *block_hash {
            self.cache = RandomXCache::new(self.flags, block_hash.as_bytes())
                .expect("Failed to create RandomX cache");
            self.vm =
                RandomXVM::new(self.flags, Some(self.cache.clone()), None)
                    .expect("Failed to create RandomX VM");
            self.current_block_hash = *block_hash;
        }
    }

    fn compute_hash(&self, nonce: &U256, block_hash: &H256) -> H256 {
        let mut input = [0u8; 64];
        input[..32].copy_from_slice(block_hash.as_bytes());
        nonce.to_little_endian(&mut input[32..64]);

        let hash = self
            .vm
            .calculate_hash(&input)
            .expect("Failed to calculate hash");
        H256::from_slice(&hash)
    }
}

// Add new struct for atomic state snapshot
struct AtomicStateSnapshot {
    block_height: u64,
    block_hash: [u64; 4],
}

// Add method to AtomicProblemState
impl AtomicProblemState {
    fn snapshot(&self) -> AtomicStateSnapshot {
        AtomicStateSnapshot {
            block_height: self.block_height.load(Ordering::Acquire),
            block_hash: [
                self.block_hash[0].load(Ordering::Acquire),
                self.block_hash[1].load(Ordering::Acquire),
                self.block_hash[2].load(Ordering::Acquire),
                self.block_hash[3].load(Ordering::Acquire),
            ],
        }
    }

    fn update(&self, problem: &ProofOfWorkProblem) {
        self.block_height
            .store(problem.block_height, Ordering::Release);
        let hash_bytes = problem.block_hash.as_bytes();
        for i in 0..4 {
            let val = u64::from_le_bytes(
                hash_bytes[i * 8..(i + 1) * 8].try_into().unwrap(),
            );
            self.block_hash[i].store(val, Ordering::Release);
        }
    }
}

impl Miner {
    pub fn new(
        num_threads: usize, worker_id: usize,
    ) -> (Self, broadcast::Receiver<(ProofOfWorkSolution, u64)>) {
        let (stratum_tx, rx) = broadcast::channel(32);
        let (solution_tx, solution_rx) = mpsc::channel();

        let state = Arc::new(RwLock::new(MiningState {
            current_problem: None,
        }));

        let atomic_state = Arc::new(AtomicProblemState {
            block_height: AtomicU64::new(0),
            block_hash: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
            is_active: AtomicBool::new(false),
        });

        let miner = Miner {
            worker_id,
            worker_name: format!("worker-{}", worker_id),
            num_threads,
            state: Arc::clone(&state),
            atomic_state: Arc::clone(&atomic_state),
            solution_sender: solution_tx,
        };

        // Spawn solution handling thread
        let worker_name = miner.worker_name.clone();
        thread::spawn(move || {
            while let Ok((solution, problem)) = solution_rx.recv() {
                // Single lock acquisition for state update
                let should_clear_problem = {
                    let state_guard = state.read().unwrap();
                    state_guard.current_problem == Some(problem.clone())
                };

                if let Err(e) =
                    stratum_tx.send((solution, problem.block_height))
                {
                    warn!(
                        "[{}] Failed to send solution to stratum: {}",
                        worker_name, e
                    );
                } else if should_clear_problem {
                    // Only acquire write lock if we need to clear the problem
                    state.write().unwrap().current_problem = None;
                }
            }
        });

        miner.spawn_mining_threads();
        (miner, rx)
    }

    fn calculate_nonce_range(
        thread_id: usize, num_threads: usize, boundary: &U256,
    ) -> (U256, U256) {
        // Focus on lower nonce ranges first
        let range_size = U256::from(boundary) / U256::from(num_threads);
        let start = range_size * U256::from(thread_id);
        let end = if thread_id == num_threads - 1 {
            U256::from(u64::MAX)
        } else {
            start + range_size
        };
        (start, end)
    }

    fn spawn_mining_threads(&self) {
        let core_ids =
            core_affinity::get_core_ids().expect("Failed to get core IDs");
        let core_count = core_ids.len();

        for i in 0..self.num_threads {
            let state = Arc::clone(&self.state);
            let worker_name = self.worker_name.clone();
            let num_threads = self.num_threads;
            let solution_sender = self.solution_sender.clone();
            let core_id = core_ids[i % core_count];
            let atomic_state = Arc::clone(&self.atomic_state);

            info!(
                "[{}] Spawning mining thread {} on core {}",
                worker_name, i, core_id.id
            );

            thread::spawn(move || {
                // Pin thread to specific core
                core_affinity::set_for_current(core_id);

                thread::sleep(Duration::from_millis(
                    ((CYCLE_LENGTH as usize / num_threads) * i) as u64,
                ));

                info!(
                    "[{}] Mining thread {} started on core {}",
                    worker_name, i, core_id.id
                );

                let mut last_log_time = Instant::now();

                let flags = RandomXFlag::get_recommended_flags();
                let mut vm = ThreadLocalVM::new(flags, &H256::zero());

                let mut current_problem: Option<LocalProblemState> = None;
                let mut hasher = BatchHasher::new();

                loop {
                    let (problem, sender) = {
                        let state_guard = state.read().unwrap();
                        (
                            state_guard.current_problem.clone(),
                            solution_sender.clone(),
                        )
                    };

                    match (problem, sender) {
                        (Some(problem), sender) => {
                            if current_problem.as_ref().map(|p| &p.problem)
                                != Some(&problem)
                            {
                                current_problem = Some(LocalProblemState::new(
                                    problem.clone(),
                                ));
                                info!(
                                    "[{}] Thread {}: New problem received, block_height: {}",
                                    worker_name, i, problem.block_height
                                );

                                vm.update_if_needed(&problem.block_hash);

                                let (start_nonce, end_nonce) =
                                    Self::calculate_nonce_range(
                                        i,
                                        num_threads,
                                        &problem.boundary,
                                    );
                                let mut current_nonce = start_nonce;
                                last_log_time = Instant::now();

                                while current_nonce < end_nonce {
                                    // Check if the problem has changed
                                    if current_nonce.low_u64() % CHECK_INTERVAL
                                        == 0
                                    {
                                        let current_height = atomic_state
                                            .block_height
                                            .load(Ordering::Acquire);
                                        if current_height
                                            != problem.block_height
                                        {
                                            break;
                                        }

                                        if !Self::check_hash_match(
                                            &problem.block_hash,
                                            &atomic_state.snapshot(),
                                        ) {
                                            break;
                                        }
                                    }

                                    let hashes = hasher.compute_hash_batch(
                                        &vm.vm,
                                        current_nonce,
                                        &problem.block_hash,
                                    );

                                    let mut solution_found = false;
                                    for (i, hash) in hashes.iter().enumerate() {
                                        let hash_u256 =
                                            U256::from(hash.as_bytes());

                                        if hash_u256 <= problem.boundary {
                                            solution_found =
                                                Self::send_solution(
                                                    &sender,
                                                    &worker_name,
                                                    i,
                                                    current_nonce + i,
                                                    &problem,
                                                );
                                        }
                                    }

                                    if solution_found {
                                        break; // Break from main mining loop
                                    }

                                    // Increment nonce after processing batch
                                    current_nonce = current_nonce
                                        .overflowing_add(U256::from(BATCH_SIZE))
                                        .0;
                                }
                            }
                        }
                        _ => {
                            if last_log_time.elapsed() >= Duration::from_secs(1)
                            {
                                info!(
                                    "[{}] Thread {}: No problem, yielding",
                                    worker_name, i
                                );
                                last_log_time = Instant::now();
                            }
                            thread::yield_now();
                        }
                    }
                }
            });
        }
    }

    pub fn mine(&self, problem: &ProofOfWorkProblem) {
        self.atomic_state
            .block_height
            .store(problem.block_height, Ordering::Release);

        let hash_bytes = problem.block_hash.as_bytes();
        for i in 0..4 {
            let val = u64::from_le_bytes(
                hash_bytes[i * 8..(i + 1) * 8].try_into().unwrap(),
            );
            self.atomic_state.block_hash[i].store(val, Ordering::Release);
        }

        // Memory fence to ensure all hash bytes are written before setting active
        atomic::fence(Ordering::Release);
        self.atomic_state.is_active.store(true, Ordering::Release);

        // Update RwLock state only for solution handling
        let mut state = self.state.write().unwrap();
        state.current_problem = Some(problem.clone());
    }

    pub fn parse_job(
        &self, params: &[Value],
    ) -> Result<ProofOfWorkProblem, String> {
        if params.len() < 4 {
            return Err("Invalid job data: not enough parameters".into());
        }

        let pow_hash_str =
            params[2].as_str().ok_or("Invalid pow_hash: not a string")?;
        let boundary_str =
            params[3].as_str().ok_or("Invalid boundary: not a string")?;

        let pow_hash = H256::from_slice(
            &hex::decode(pow_hash_str.trim_start_matches("0x"))
                .map_err(|e| format!("Invalid pow_hash: {}", e))?,
        );

        let boundary = U256::from_str(boundary_str.trim_start_matches("0x"))
            .map_err(|e| format!("Invalid boundary: {}", e))?;

        let block_height = params[1]
            .as_str()
            .ok_or("Invalid block height: not a string")?
            .parse::<u64>()
            .map_err(|e| format!("Invalid block height: {}", e))?;

        let difficulty = boundary_to_difficulty(&boundary);

        info!(
            "Parsed job: block_height={}, pow_hash={:.4}…{:.4}, boundary=0x{:x}, calculated difficulty={}",
            block_height,
            pow_hash,
            hex::encode(&pow_hash.as_bytes()[28..32]),
            boundary,
            difficulty
        );

        let problem = ProofOfWorkProblem::new_from_boundary(
            block_height,
            pow_hash,
            boundary,
        );
        info!(
            "Created ProofOfWorkProblem with boundary: 0x{:x}, difficulty: {}",
            problem.boundary, problem.difficulty
        );
        Ok(problem)
    }

    fn check_hash_match(
        block_hash: &H256, atomic_state: &AtomicStateSnapshot,
    ) -> bool {
        let hash_bytes = block_hash.as_bytes();
        for i in 0..4 {
            let stored = atomic_state.block_hash[i];
            let expected = u64::from_le_bytes(
                hash_bytes[i * 8..(i + 1) * 8].try_into().unwrap(),
            );
            if stored != expected {
                return false;
            }
        }

        true
    }

    fn send_solution(
        solution_sender: &mpsc::Sender<(
            ProofOfWorkSolution,
            ProofOfWorkProblem,
        )>,
        worker_name: &str, i: usize, solution_nonce: U256,
        problem: &ProofOfWorkProblem,
    ) -> bool {
        let solution = ProofOfWorkSolution {
            nonce: solution_nonce,
        };

        match solution_sender.send((solution, problem.clone())) {
            Ok(_) => {
                info!(
                "[{}] Thread {}: Successfully sent solution with nonce {} for block {}",
                    worker_name, i, solution_nonce, problem.block_height
                );
                return true;
            }
            Err(e) => {
                warn!(
                    "[{}] Thread {}: Failed to send solution: {}",
                    worker_name, i, e
                );
                return false;
            }
        }
    }
}
