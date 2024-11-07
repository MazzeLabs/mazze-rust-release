use core_affinity::{self, CoreId};
use log::{info, warn};
use mazze_types::{H256, U256};
use mazzecore::pow::{
    boundary_to_difficulty, ProofOfWorkProblem, ProofOfWorkSolution,
};
use randomx_rs::RandomXFlag;
use serde_json::Value;
use std::mem;
use std::str::FromStr;
use std::sync::atomic::{self, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use tokio::sync::broadcast;

use crate::core::*;

const CHECK_INTERVAL: u64 = 2; // Check for new problem every 2 nonces

const BATCH_SIZE: usize = 8;

#[derive(Clone)]
pub struct Miner {
    pub worker_id: usize,
    pub worker_name: String,
    num_threads: usize,
    atomic_state: Arc<AtomicProblemState>,
    solution_sender: mpsc::Sender<(ProofOfWorkSolution, u64)>,
}

impl Miner {
    pub fn new(
        num_threads: usize, worker_id: usize,
    ) -> (Self, broadcast::Receiver<(ProofOfWorkSolution, u64)>) {
        let (stratum_tx, rx) = broadcast::channel(32);
        let (solution_tx, solution_rx) = mpsc::channel();

        let atomic_state = Arc::new(AtomicProblemState::default());

        let miner = Miner {
            worker_id,
            worker_name: format!("worker-{}", worker_id),
            num_threads,
            atomic_state: Arc::clone(&atomic_state),
            solution_sender: solution_tx,
        };

        // Spawn solution handling thread
        let worker_name = miner.worker_name.clone();
        thread::spawn(move || {
            while let Ok((solution, block_height)) = solution_rx.recv() {
                // TODO: add hash check here
                if let Err(e) = stratum_tx.send((solution, block_height)) {
                    warn!(
                        "[{}] Failed to send solution to stratum: {}",
                        worker_name, e
                    );
                }
            }
        });

        miner.spawn_mining_threads();
        (miner, rx)
    }

    pub fn mine(&mut self, problem: &ProofOfWorkProblem) {
        // Create new atomic state from problem
        let mut new_state = AtomicProblemState::new(
            problem.block_height,
            problem.block_hash,
            problem.boundary,
        );

        // Swap the new state with the old one atomically
        let old_state = Arc::get_mut(&mut self.atomic_state)
            .expect("Cannot get mutable reference to atomic state");
        mem::swap(old_state, &mut new_state);

        // Ensure all threads see the new state
        atomic::fence(Ordering::Release);
    }

    pub fn parse_job(
        &mut self, params: &[Value],
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

        // Immediately update the atomic state
        self.mine(&problem);

        Ok(problem)
    }

    fn spawn_mining_threads(&self) {
        // Setup core affinity
        let core_ids = Self::setup_core_affinity();

        // Spawn threads
        for thread_id in 0..self.num_threads {
            self.spawn_mining_thread(thread_id, &core_ids);
        }
    }

    fn setup_core_affinity() -> Vec<CoreId> {
        core_affinity::get_core_ids().expect("Failed to get core IDs")
    }

    fn spawn_mining_thread(&self, thread_id: usize, core_ids: &[CoreId]) {
        let worker_name = self.worker_name.clone();
        let num_threads = self.num_threads;
        let solution_sender = self.solution_sender.clone();
        let core_id = core_ids[thread_id % core_ids.len()];
        let atomic_state = Arc::clone(&self.atomic_state);

        info!(
            "[{}] Spawning mining thread {} on core {}",
            worker_name, thread_id, core_id.id
        );

        thread::spawn(move || {
            Miner::run_mining_thread(
                thread_id,
                core_id,
                worker_name,
                num_threads,
                solution_sender,
                atomic_state,
            );
        });
    }

    fn run_mining_thread(
        thread_id: usize, core_id: CoreId, worker_name: String,
        num_threads: usize,
        solution_sender: mpsc::Sender<(ProofOfWorkSolution, u64)>,
        atomic_state: Arc<AtomicProblemState>,
    ) {
        // Pin thread to core
        Miner::setup_thread_affinity(core_id, &worker_name, thread_id);

        // Initialize mining components
        let flags = RandomXFlag::get_recommended_flags();
        let mut vm = ThreadLocalVM::new(flags, &H256::zero());
        let mut hasher = BatchHasher::new();
        let current_state = AtomicProblemState::default();

        // Initialize SIMD boundary for x86_64
        #[cfg(target_arch = "x86_64")]
        let mut simd_boundary = unsafe { SIMDBoundary::new(&atomic_state) };

        // Main mining loop
        loop {
            // Get current problem details atomically
            let (height, block_hash, _boundary) =
                atomic_state.get_problem_details();

            // Check for new problem
            if current_state != *atomic_state {
                info!(
                    "[{}] Thread {}: New problem received, block_height: {}",
                    worker_name, thread_id, height
                );

                // Update current state and VM
                current_state.update(&atomic_state);
                vm.update_if_needed(&block_hash);

                // Update SIMD boundary when problem changes (x86_64 only)
                #[cfg(target_arch = "x86_64")]
                unsafe {
                    simd_boundary = SIMDBoundary::new(&atomic_state);
                }

                // Calculate nonce range for this thread
                let (start_nonce, end_nonce) =
                    current_state.calculate_nonce_range(thread_id, num_threads);
                let mut current_nonce = start_nonce;

                while current_nonce < end_nonce {
                    // Check if problem changed
                    if current_nonce.low_u64() % CHECK_INTERVAL == 0
                        && current_state != *atomic_state
                    {
                        break;
                    }

                    // Process batch and check for solutions
                    #[cfg(target_arch = "x86_64")]
                    let solution = Miner::process_hash_batch(
                        &mut vm,
                        &mut hasher,
                        current_nonce,
                        &block_hash,
                        &simd_boundary,
                    );

                    #[cfg(not(target_arch = "x86_64"))]
                    let solution = Miner::process_hash_batch(
                        &mut vm,
                        &mut hasher,
                        current_nonce,
                        &block_hash,
                        &atomic_state,
                    );

                    if let Some(solution) = solution {
                        if let Err(e) = solution_sender.send((solution, height))
                        {
                            warn!(
                                "[{}] Thread {}: Failed to send solution: {}",
                                worker_name, thread_id, e
                            );
                        }
                    }

                    current_nonce =
                        current_nonce.overflowing_add(U256::from(BATCH_SIZE)).0;
                }
            }

            thread::yield_now();
        }
    }

    fn setup_thread_affinity(
        core_id: CoreId, worker_name: &str, thread_id: usize,
    ) {
        core_affinity::set_for_current(core_id);
        info!(
            "[{}] Mining thread {} started on core {}",
            worker_name, thread_id, core_id.id
        );
    }

    fn initialize_mining_vm() -> ThreadLocalVM {
        let flags = RandomXFlag::get_recommended_flags();
        ThreadLocalVM::new(flags, &H256::zero())
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn process_hash_batch(
        vm: &mut ThreadLocalVM, hasher: &mut BatchHasher, current_nonce: U256,
        block_hash: &H256, atomic_state: &AtomicProblemState,
    ) -> Option<ProofOfWorkSolution> {
        let hashes = hasher.compute_hash_batch(vm, current_nonce, block_hash);
        let boundary = atomic_state.get_boundary();

        for (i, hash) in hashes.iter().enumerate() {
            let hash_u256 = U256::from(hash.as_bytes());
            if hash_u256 <= boundary {
                return Some(ProofOfWorkSolution {
                    nonce: current_nonce + i,
                });
            }
        }

        None
    }

    #[cfg(target_arch = "x86_64")]
    fn process_hash_batch(
        vm: &mut ThreadLocalVM, hasher: &mut BatchHasher, current_nonce: U256,
        block_hash: &H256, simd_boundary: &SIMDBoundary,
    ) -> Option<ProofOfWorkSolution> {
        let hashes =
            hasher.compute_hash_batch(&vm.vm, current_nonce, block_hash);

        unsafe {
            // Check each hash in the batch using the cached SIMD boundary
            for (i, hash) in hashes.iter().enumerate() {
                if simd_boundary.compare_hash(hash) {
                    return Some(ProofOfWorkSolution {
                        nonce: current_nonce + i,
                    });
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex::FromHex;
    use std::time::{Duration, Instant};

    #[test]
    fn test_mining_state_transitions() {
        // Create initial state
        let state1 =
            AtomicProblemState::new(1, H256::random(), U256::from(1000000));

        // Create newer state
        let state2 =
            AtomicProblemState::new(2, H256::random(), U256::from(1000000));

        // Test ordering
        assert!(
            state1 < state2,
            "State with higher block height should be greater"
        );

        // Test equality with problem
        let problem = ProofOfWorkProblem::new_from_boundary(
            1,
            state1.get_block_hash(),
            state1.get_boundary(),
        );
        assert_eq!(state1, problem, "State should equal equivalent problem");

        // Test state updates
        let current_state = AtomicProblemState::default();
        assert!(
            current_state < state1,
            "Default state should be less than any valid state"
        );

        current_state.update(&state1);
        assert_eq!(current_state, state1, "State should equal after update");

        assert!(
            current_state < state2,
            "Updated state should be less than newer state"
        );
    }

    #[test]
    fn test_concurrent_state_transitions() {
        let atomic_state = Arc::new(AtomicProblemState::default());
        let thread_count = 4;
        let iterations = 1000;

        let mut handles = vec![];
        let start = Instant::now();

        for _thread_id in 0..thread_count {
            let state = Arc::clone(&atomic_state);
            handles.push(thread::spawn(move || {
                let local_state = AtomicProblemState::default();
                let mut transitions = 0;

                for _i in 0..iterations {
                    if local_state != *state {
                        local_state.update(&state);
                        transitions += 1;
                    }
                    thread::yield_now();
                }
                transitions
            }));
        }

        // Update shared state a few times
        for i in 1..=5 {
            thread::sleep(Duration::from_millis(10));
            let new_state =
                AtomicProblemState::new(i, H256::random(), U256::from(1000000));
            atomic_state.update(&new_state);
        }

        let total_transitions: usize =
            handles.into_iter().map(|h| h.join().unwrap()).sum();

        println!(
            "Completed {} state transitions across {} threads in {:?}",
            total_transitions,
            thread_count,
            start.elapsed()
        );
        assert!(
            total_transitions > 0,
            "Should have detected state transitions"
        );
    }

    #[test]
    fn test_boundary_conversions() {
        // Test boundary from hex string
        let boundary_hex =
            "3fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        let boundary = U256::from_str(boundary_hex).unwrap();

        // Convert to atomic state format and back
        let mut atomic_state = AtomicProblemState::default();
        for i in 0..4 {
            let chunk = boundary.0[i];
            atomic_state.boundary[i].store(chunk, Ordering::Relaxed);
        }

        let recovered_boundary = atomic_state.get_boundary();
        assert_eq!(boundary, recovered_boundary, "Boundary conversion failed");
    }

    #[test]
    fn test_block_hash_conversions() {
        // Test block hash from hex string
        let block_hash_hex =
            "7dc6e0aad8b74e5ee04e2f34e01b457d017bc4c38c7a5db001e5c7baecbab4e8";
        let block_hash =
            H256::from_slice(&Vec::from_hex(block_hash_hex).unwrap());

        // Convert to bytes and back
        let bytes = block_hash.as_bytes();
        let recovered_hash = H256::from_slice(bytes);

        assert_eq!(block_hash, recovered_hash, "Block hash conversion failed");
    }

    #[test]
    fn test_nonce_validation() {
        // Setup test data
        let block_hash_hex =
            "7dc6e0aad8b74e5ee04e2f34e01b457d017bc4c38c7a5db001e5c7baecbab4e8";
        let block_hash =
            H256::from_slice(&Vec::from_hex(block_hash_hex).unwrap());

        let boundary_hex =
            "3fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        let boundary = U256::from_str(boundary_hex).unwrap();

        let nonce = U256::from_dec_str("14474011154664524427946373126085988481658748083205070504932198000989141204990").unwrap();

        // Setup VM and hasher
        let flags = RandomXFlag::get_recommended_flags();
        let vm = ThreadLocalVM::new(flags, &block_hash);
        let mut hasher = BatchHasher::new();

        // Test single nonce validation
        let hashes = hasher.compute_hash_batch(&vm.vm, nonce, &block_hash);
        let hash = &hashes[0];
        let hash_u256 = U256::from(hash.as_bytes());

        assert!(hash_u256 <= boundary, "Known valid nonce failed validation");
    }

    #[test]
    fn test_simd_boundary_comparison() {
        #[cfg(target_arch = "x86_64")]
        {
            // Setup test data
            let boundary_hex = "3fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
            let boundary = U256::from_str(boundary_hex).unwrap();

            // Create atomic state with known boundary
            let mut atomic_state = AtomicProblemState::default();
            for i in 0..4 {
                atomic_state.boundary[i]
                    .store(boundary.0[i], Ordering::Relaxed);
            }

            // Create SIMD boundary
            let simd_boundary = unsafe { SIMDBoundary::new(&atomic_state) };

            // Test known valid hash
            let block_hash_hex = "7dc6e0aad8b74e5ee04e2f34e01b457d017bc4c38c7a5db001e5c7baecbab4e8";
            let block_hash =
                H256::from_slice(&Vec::from_hex(block_hash_hex).unwrap());

            let flags = RandomXFlag::get_recommended_flags();
            let vm = ThreadLocalVM::new(flags, &block_hash);
            let mut hasher = BatchHasher::new();

            let nonce = U256::from_dec_str("14474011154664524427946373126085988481658748083205070504932198000989141204990").unwrap();
            let hashes = hasher.compute_hash_batch(&vm.vm, nonce, &block_hash);

            unsafe {
                assert!(
                    simd_boundary.compare_hash(&hashes[0]),
                    "SIMD comparison failed for known valid hash"
                );
            }
        }
    }
}

use std::arch::x86_64::__m256i;

#[cfg(target_arch = "x86_64")]
struct SIMDBoundary {
    boundary_vec: __m256i,
}

#[cfg(target_arch = "x86_64")]
impl SIMDBoundary {
    unsafe fn new(atomic_state: &AtomicProblemState) -> Self {
        use std::arch::x86_64::_mm256_loadu_si256;

        // Load boundary chunks with Relaxed ordering since we'll reuse it
        let mut boundary_bytes = [0u8; 32];
        for i in 0..4 {
            let chunk = atomic_state.boundary[i].load(Ordering::Relaxed);
            boundary_bytes[i * 8..(i + 1) * 8]
                .copy_from_slice(&chunk.to_le_bytes());
        }

        Self {
            boundary_vec: _mm256_loadu_si256(
                boundary_bytes.as_ptr() as *const __m256i
            ),
        }
    }

    #[inline]
    unsafe fn compare_hash(&self, hash: &H256) -> bool {
        use std::arch::x86_64::{
            _mm256_cmpgt_epi8, _mm256_loadu_si256, _mm256_set1_epi8,
            _mm256_testc_si256,
        };

        let hash_vec =
            _mm256_loadu_si256(hash.as_bytes().as_ptr() as *const __m256i);
        let cmp = _mm256_cmpgt_epi8(self.boundary_vec, hash_vec);
        _mm256_testc_si256(cmp, _mm256_set1_epi8(-1)) != 0
    }
}
