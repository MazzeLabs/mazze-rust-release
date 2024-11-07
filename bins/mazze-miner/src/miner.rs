use core_affinity;
use log::{info, trace, warn};
use mazze_types::{H256, U256};
use mazzecore::pow::{
    boundary_to_difficulty, ProofOfWorkProblem, ProofOfWorkSolution,
};
use randomx_rs::{RandomXCache, RandomXFlag, RandomXVM};
use serde_json::Value;
use std::mem;
use std::str::FromStr;
use std::sync::atomic::{self, AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

use crate::core::*;

const CHECK_INTERVAL: u64 = 2; // Check for new problem every 2 nonces

const BATCH_SIZE: usize = 8;

struct MiningState {
    current_problem: Option<ProofOfWorkProblem>,
}

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
        let atomic_state = Arc::clone(&atomic_state);
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
        let core_ids =
            core_affinity::get_core_ids().expect("Failed to get core IDs");
        let core_count = core_ids.len();

        for i in 0..self.num_threads {
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

                info!(
                    "[{}] Mining thread {} started on core {}",
                    worker_name, i, core_id.id
                );

                let flags = RandomXFlag::get_recommended_flags();
                let mut vm = ThreadLocalVM::new(flags, &H256::zero());

                let mut hasher = BatchHasher::new();

                let current_problem =
                    AtomicProblemState::from_problem(&atomic_state);

                loop {
                    // Get current problem details atomically
                    let (height, block_hash, boundary) =
                        atomic_state.get_problem_details();

                    if current_problem.block_height.load(Ordering::Relaxed)
                        != height
                    {
                        info!(
                                "[{}] Thread {}: New problem received, block_height: {}",
                                worker_name, i, height
                            );

                        current_problem.update(&atomic_state);
                        vm.update_if_needed(&block_hash);

                        let (start_nonce, end_nonce) = current_problem
                            .calculate_nonce_range(i, num_threads);
                        let mut current_nonce = start_nonce;

                        while current_nonce < end_nonce {
                            // Check if problem changed
                            if current_nonce.low_u64() % CHECK_INTERVAL == 0 {
                                let (new_height, _, _) =
                                    atomic_state.get_problem_details();
                                if new_height != height {
                                    break;
                                }
                            }

                            let hashes = hasher.compute_hash_batch(
                                &vm.vm,
                                current_nonce,
                                &block_hash,
                            );

                            for (i, hash) in hashes.iter().enumerate() {
                                let hash_u256 = U256::from(hash.as_bytes());
                                if hash_u256 <= boundary {
                                    let solution = ProofOfWorkSolution {
                                        nonce: current_nonce + i,
                                    };
                                    if let Err(e) =
                                        solution_sender.send((solution, height))
                                    {
                                        warn!(
                                                "[{}] Thread {}: Failed to send solution: {}",
                                                worker_name, i, e
                                            );
                                    }
                                    break;
                                }
                            }

                            current_nonce = current_nonce
                                .overflowing_add(U256::from(BATCH_SIZE))
                                .0;
                        }
                    }

                    thread::yield_now();
                }
            });
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;
    use std::time::Instant;

    #[test]
    fn benchmark_equality_comparison() {
        let test_cases = 1_000_000;

        // Create two identical problems for best-case scenario
        let problem1 = AtomicProblemState::new(
            1,
            H256::random(), // Use random hash for realistic test
            U256::from(1000000),
        );
        let problem2 = AtomicProblemState::from_problem(&problem1);

        // Benchmark SIMD version
        let start = Instant::now();
        for _ in 0..test_cases {
            let _ = problem1.eq(&problem2);
        }
        let simd_duration = start.elapsed();

        // Benchmark scalar version (forcing non-SIMD path)
        let start = Instant::now();
        for _ in 0..test_cases {
            let _ = problem1.get_block_hash() == problem2.get_block_hash()
                && problem1.get_boundary() == problem2.get_boundary();
        }
        let scalar_duration = start.elapsed();

        println!("Equality comparison over {} iterations:", test_cases);
        println!("SIMD implementation: {:?}", simd_duration);
        println!("Scalar implementation: {:?}", scalar_duration);
        println!(
            "Speedup factor: {:.2}x",
            scalar_duration.as_nanos() as f64 / simd_duration.as_nanos() as f64
        );
    }

    #[test]
    fn test_hash_match_correctness_and_performance() {
        // Create test data
        let mut rng = rand::thread_rng();
        let test_cases = 1_000_000; // Number of test iterations

        // Generate random test data
        let mut test_data: Vec<(H256, AtomicStateSnapshot)> =
            Vec::with_capacity(test_cases);
        for _ in 0..test_cases {
            // Generate random hash
            let mut hash_bytes = [0u8; 32];
            rng.fill(&mut hash_bytes);
            let block_hash = H256::from_slice(&hash_bytes);

            // Create matching atomic state
            let mut block_hash_chunks = [0u64; 4];
            for i in 0..4 {
                block_hash_chunks[i] = u64::from_le_bytes(
                    hash_bytes[i * 8..(i + 1) * 8].try_into().unwrap(),
                );
            }

            let atomic_state = AtomicStateSnapshot {
                block_height: 0,
                block_hash: block_hash_chunks,
            };

            test_data.push((block_hash, atomic_state));
        }

        // Test correctness
        for (block_hash, atomic_state) in test_data.iter() {
            #[cfg(target_arch = "x86_64")]
            {
                let simd_result =
                    Miner::check_hash_match(block_hash, atomic_state);
                assert!(
                    simd_result,
                    "SIMD implementation failed to match identical hashes"
                );
            }

            // Test non-matching case
            let mut modified_state = atomic_state.clone();
            modified_state.block_hash[0] ^= 1; // Flip one bit
            #[cfg(target_arch = "x86_64")]
            {
                let simd_result =
                    Miner::check_hash_match(block_hash, &modified_state);
                assert!(
                    !simd_result,
                    "SIMD implementation failed to detect different hashes"
                );
            }
        }

        // Benchmark performance
        let start_time = Instant::now();
        for (block_hash, atomic_state) in test_data.iter() {
            #[cfg(target_arch = "x86_64")]
            {
                let _ = Miner::check_hash_match(block_hash, atomic_state);
            }
        }
        let simd_duration = start_time.elapsed();

        // Implement and test the non-SIMD version for comparison
        fn check_hash_match_sequential(
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

        let start_time = Instant::now();
        for (block_hash, atomic_state) in test_data.iter() {
            let _ = check_hash_match_sequential(block_hash, atomic_state);
        }
        let sequential_duration = start_time.elapsed();

        println!("Performance comparison over {} iterations:", test_cases);
        println!("SIMD implementation: {:?}", simd_duration);
        println!("Sequential implementation: {:?}", sequential_duration);
        println!(
            "Speedup factor: {:.2}x",
            sequential_duration.as_nanos() as f64
                / simd_duration.as_nanos() as f64
        );
    }

    #[cfg(target_arch = "x86_64")]
    fn check_hash_match(
        block_hash: &H256, atomic_state: &AtomicStateSnapshot,
    ) -> bool {
        use std::arch::x86_64::{
            __m256i, _mm256_cmpeq_epi64, _mm256_loadu_si256,
            _mm256_set1_epi64x, _mm256_testc_si256,
        };

        unsafe {
            // Load the entire atomic state hash (32 bytes) as a 256-bit vector
            let atomic_hash = _mm256_loadu_si256(
                atomic_state.block_hash.as_ptr() as *const __m256i,
            );

            // Load the block hash as a 256-bit vector
            let expected_hash = _mm256_loadu_si256(
                block_hash.as_bytes().as_ptr() as *const __m256i,
            );

            // Compare the two vectors for equality
            // _mm256_cmpeq_epi64 returns -1 (all ones) for equal elements
            let cmp = _mm256_cmpeq_epi64(atomic_hash, expected_hash);

            // Check if all elements are equal (all bits are 1)
            _mm256_testc_si256(cmp, _mm256_set1_epi64x(-1)) == 1
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
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

    #[test]
    fn test_atomic_problem_state_comparisons() {
        let problem1 = AtomicProblemState::new(1, H256::zero(), U256::zero());

        let problem2 = AtomicProblemState::new(2, H256::zero(), U256::zero());

        // Test ordering
        assert!(problem1 < problem2);
        assert!(problem2 > problem1);
        assert!(problem1 <= problem2);
        assert!(problem2 >= problem1);

        // Test equality
        let problem1_clone =
            AtomicProblemState::new(1, H256::zero(), U256::zero());
        assert_eq!(problem1, problem1_clone);

        // Test comparison with ProofOfWorkProblem
        let pow_problem = ProofOfWorkProblem::new_from_boundary(
            1,
            H256::zero(),
            U256::zero(),
        );
        assert_eq!(problem1, pow_problem);
        assert!(problem2 > pow_problem);
    }
}
