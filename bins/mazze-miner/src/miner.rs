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

/*
Flow:
Writer (mine thread)                    Reader (mining threads)
─────────────────────                  ─────────────────────
prepare new state
│                                      read current ptr ──┐
atomic ptr swap ───────────────────►   use state data    │
                                      compare states    ◄─┘
*/

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
        // Create new state (already handles endianness)
        let new_state = ProblemState::from(problem);

        self.atomic_state.update(new_state);
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
        let mut current_generation = 0;

        // Main mining loop
        loop {
            let state_generation = atomic_state.get_generation();
            if current_generation != state_generation {
                current_generation = state_generation;

                let (height, block_hash, _) =
                    atomic_state.get_problem_details();

                info!(
                    "[{}] Thread {}: New problem received, block_height: {}",
                    worker_name, thread_id, height
                );

                vm.update_if_needed(&block_hash);

                // Calculate nonce range for this thread
                let (start_nonce, end_nonce) =
                    atomic_state.calculate_nonce_range(thread_id, num_threads);
                let mut current_nonce = start_nonce;

                while current_nonce < end_nonce {
                    // Check for new problem periodically
                    if current_nonce.low_u64() % CHECK_INTERVAL == 0 {
                        break;
                    }

                    // Process batch and check for solutions
                    #[cfg(target_arch = "x86_64")]
                    {
                        let hashes = hasher.compute_hash_batch(
                            &vm.vm,
                            current_nonce,
                            &block_hash,
                        );

                        // Use SIMD comparison
                        for (i, hash) in hashes.iter().enumerate() {
                            if atomic_state.check_hash_simd(hash) {
                                let solution = ProofOfWorkSolution {
                                    nonce: current_nonce + i,
                                };
                                if let Err(e) =
                                    solution_sender.send((solution, height))
                                {
                                    warn!(
                                    "[{}] Thread {}: Failed to send solution: {}",
                                    worker_name, thread_id, e
                                );
                                }
                            }
                        }
                    }

                    #[cfg(not(target_arch = "x86_64"))]
                    {
                        let hashes = hasher.compute_hash_batch(
                            &vm.vm,
                            current_nonce,
                            &block_hash,
                        );
                        let boundary = atomic_state.get_boundary();

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
                                    worker_name, thread_id, e
                                );
                                }
                            }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex::FromHex;
    use std::time::{Duration, Instant};

    #[test]
    fn test_mining_state_transitions() {
        // Create a single atomic state to track real transitions
        let atomic_state = AtomicProblemState::default();
        let initial_gen = atomic_state.get_generation();

        // Update with first state
        let state1 = ProblemState::new(
            1,
            H256::from([1u8; 32]), // Deterministic hash
            U256::from(1000000),
        );
        atomic_state.update(state1);
        let gen1 = atomic_state.get_generation();

        // Update with second state
        let state2 = ProblemState::new(
            2,
            H256::from([2u8; 32]), // Deterministic hash
            U256::from(1000000),
        );
        atomic_state.update(state2);
        let gen2 = atomic_state.get_generation();

        // Test generations increase with updates
        assert!(
            gen1 > initial_gen,
            "Generation should increase after first update"
        );
        assert!(
            gen2 > gen1,
            "Generation should increase after second update"
        );

        // Verify current state matches last update
        let (height, hash, boundary) = atomic_state.get_problem_details();
        assert_eq!(height, 2, "Height should match last update");
        assert_eq!(
            hash,
            H256::from([2u8; 32]),
            "Hash should match last update"
        );
        assert_eq!(
            boundary,
            U256::from(1000000),
            "Boundary should match last update"
        );
    }

    #[test]
    fn test_concurrent_state_transitions() {
        let atomic_state = Arc::new(AtomicProblemState::default());
        let thread_count = 4;
        let iterations = 100; // Reduced for clarity
        let mut handles = vec![];

        println!("Starting concurrent state transition test");
        let start = Instant::now();

        // Spawn checker threads
        for thread_id in 0..thread_count {
            let state = Arc::clone(&atomic_state);
            handles.push(thread::spawn(move || {
                let mut current_generation = 0;
                let mut transitions = 0;

                println!(
                    "Thread {} started checking for state changes",
                    thread_id
                );

                for i in 0..iterations {
                    let new_generation = state.get_generation();
                    if current_generation != new_generation {
                        println!(
                            "Thread {} detected change at iteration {}",
                            thread_id, i
                        );
                        current_generation = new_generation;
                        transitions += 1;
                    }
                    thread::sleep(Duration::from_millis(1)); // Small sleep to reduce CPU usage
                }

                println!(
                    "Thread {} finished with {} transitions",
                    thread_id, transitions
                );
                transitions
            }));
        }

        // Ensure threads have started
        thread::sleep(Duration::from_millis(50));

        println!("Starting state updates");
        // Update shared state a few times
        for i in 1..=5 {
            let new_state = ProblemState::new(
                i,
                H256::from([i as u8; 32]), // Deterministic hash based on i
                U256::from(1000000),
            );
            println!("Updating state to block height {}", i);
            atomic_state.update(new_state);
            thread::sleep(Duration::from_millis(50)); // Give more time for detection
        }

        let transitions: Vec<usize> =
            handles.into_iter().map(|h| h.join().unwrap()).collect();

        let total_transitions: usize = transitions.iter().sum();

        println!("Test completed in {:?}", start.elapsed());
        println!("Transitions per thread: {:?}", transitions);
        println!("Total transitions: {}", total_transitions);

        // We expect each thread to see at least one change
        assert!(
            total_transitions >= thread_count,
            "Each thread should detect at least one state transition. \
             Expected at least {} total transitions, got {} \
             (transitions per thread: {:?})",
            thread_count,
            total_transitions,
            transitions
        );
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
    fn test_concurrent_mining() {
        let (mut miner, _rx) = Miner::new(4, 1);

        // Create and submit a problem
        let problem = ProofOfWorkProblem::new_from_boundary(
            1,
            H256::random(),
            U256::from(1000000),
        );

        miner.mine(&problem);

        // Let it mine for a bit
        thread::sleep(Duration::from_secs(1));

        // Submit a new problem
        let new_problem = ProofOfWorkProblem::new_from_boundary(
            2,
            H256::random(),
            U256::from(1000000),
        );

        miner.mine(&new_problem);

        // Verify all threads picked up the new problem
        thread::sleep(Duration::from_millis(100));

        let (height, _, _) = miner.atomic_state.get_problem_details();
        assert_eq!(height, 2, "All threads should be mining the new problem");
    }
}
