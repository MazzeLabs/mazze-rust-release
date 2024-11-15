use core_affinity::{self, CoreId};
use log::error;
use log::{debug, info, trace, warn};
use mazze_types::{H256, U256};
use mazzecore::pow::{
    boundary_to_difficulty, ProofOfWorkProblem, ProofOfWorkSolution,
};
use randomx_rs::RandomXFlag;
use serde_json::Value;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::{mpsc, Barrier};
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::broadcast;

use crate::core::*;
use crate::core_numa::NumaError;
use crate::core_numa::ThreadAssignment;
use crate::core_numa::{NewNumaVMManager, THREAD_VM};
use crate::mining_metrics::MiningMetrics;

const CHECK_INTERVAL: u64 = 64 * BATCH_SIZE as u64;

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
    solution_sender: mpsc::Sender<(ProofOfWorkSolution, u64)>,
    metrics: Arc<MiningMetrics>,
    vm_manager: Arc<NewNumaVMManager>,
}

impl Miner {
    pub fn new_numa(
        num_threads: usize, worker_id: usize,
    ) -> Result<
        (Self, broadcast::Receiver<(ProofOfWorkSolution, u64)>),
        NumaError,
    > {
        let (stratum_tx, rx) = broadcast::channel(32);
        let (solution_tx, solution_rx) = mpsc::channel();
        let metrics = Arc::new(MiningMetrics::new());
        let vm_manager = Arc::new(NewNumaVMManager::new()?);

        let miner = Miner {
            worker_id,
            worker_name: format!("worker-{}", worker_id),
            num_threads,
            solution_sender: solution_tx,
            metrics: Arc::clone(&metrics),
            vm_manager: Arc::clone(&vm_manager),
        };

        // Spawn solution handler
        let worker_name = miner.worker_name.clone();
        Self::spawn_solution_handler(solution_rx, stratum_tx, worker_name);

        // Spawn mining threads
        miner.spawn_numa_mining_threads()?;

        Ok((miner, rx))
    }

    pub fn mine(&mut self, problem: &ProofOfWorkProblem) {
        debug!(
            "[{}] mine() called with new height={}, hash={:.8}",
            self.worker_name,
            problem.block_height,
            hex::encode(&problem.block_hash.as_bytes()[..4])
        );

        // Update VMs before mining
        if let Err(e) = self.vm_manager.update_if_needed(problem) {
            error!("[{}] Failed to update VMs: {:?}", self.worker_name, e);
            return;
        }

        debug!("[{}] VMs updated successfully", self.worker_name);
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

        self.mine(&problem);

        Ok(problem)
    }

    fn spawn_numa_mining_threads(&self) -> Result<(), NumaError> {
        info!(
            "[{}] Spawning {} NUMA-aware mining threads",
            self.worker_name, self.num_threads
        );

        let barrier = Arc::new(Barrier::new(self.num_threads));

        for thread_id in 0..self.num_threads {
            let barrier = Arc::clone(&barrier);
            let assignment = ThreadAssignment {
                thread_id,
                node_id: thread_id % self.vm_manager.topology.get_nodes().len(),
                core_id: thread_id,
            };

            info!(
                "[{}] Assigning thread {} to NUMA node {} core {}",
                self.worker_name,
                thread_id,
                assignment.node_id,
                assignment.core_id
            );

            self.spawn_mining_thread_numa(assignment, barrier)?;
        }

        Ok(())
    }

    fn spawn_mining_thread_numa(
        &self, assignment: ThreadAssignment, barrier: Arc<Barrier>,
    ) -> Result<thread::JoinHandle<()>, NumaError> {
        let worker_name = self.worker_name.clone();
        let solution_sender = self.solution_sender.clone();
        let num_threads = self.num_threads;
        let vm_manager = self.vm_manager.clone();

        let handle = thread::spawn(move || {
            #[cfg(target_os = "linux")]
            unsafe {
                libc::nice(1);
            }

            barrier.wait();

            Self::run_mining_thread_numa(
                &assignment,
                worker_name,
                solution_sender,
                vm_manager,
                num_threads,
                barrier,
            );
        });

        Ok(handle)
    }

    fn run_mining_thread_numa(
        assignment: &ThreadAssignment, worker_name: String,
        solution_sender: mpsc::Sender<(ProofOfWorkSolution, u64)>,
        vm_manager: Arc<NewNumaVMManager>, num_threads: usize,
        barrier: Arc<Barrier>,
    ) {
        info!(
            "[{}] Starting mining thread {} on NUMA node {} core {}",
            worker_name,
            assignment.thread_id,
            assignment.node_id,
            assignment.core_id
        );

        // Set thread affinity
        #[cfg(target_os = "linux")]
        if let Some(core_ids) = core_affinity::get_core_ids() {
            if let Some(core_id) = core_ids.get(assignment.core_id) {
                core_affinity::set_for_current(*core_id);
                debug!(
                    "[{}] Set thread affinity to core {}",
                    worker_name, assignment.core_id
                );
            }
        }

        barrier.wait();
        info!(
            "[{}] Thread passed barrier, starting mining loop",
            worker_name
        );

        loop {
            let result = vm_manager.with_vm(assignment, |vm| {
                let (start_nonce, end_nonce) = Self::calculate_nonce_range(
                    assignment.thread_id,
                    num_threads,
                );
                debug!(
                    "[{}] Mining range: start={}, end={}, block={}",
                    worker_name,
                    start_nonce,
                    end_nonce,
                    vm.get_current_height()
                );

                let mut current_nonce = start_nonce;
                let mut hashes_computed = 0u64;
                let start_time = Instant::now();
                let mut blocks_processed = 0u64;
                let mut last_block_height = vm.get_current_height();

                // Mining loop
                while current_nonce < end_nonce {
                    if current_nonce.low_u64() % CHECK_INTERVAL == 0 {
                        let elapsed = start_time.elapsed();
                        if elapsed.as_secs() > 0 {
                            let hash_rate = hashes_computed as f64 / elapsed.as_secs_f64();
                            trace!(
                                "[{}] Hash rate: {:.2} H/s, Blocks: {:.2} b/s, current nonce: {}, block: {}",
                                worker_name,
                                hash_rate,
                                blocks_processed as f64 / elapsed.as_secs_f64(),
                                current_nonce,
                                vm.get_current_height()
                            );
                        }

                        debug!(
                            "[{}] Checking if block hash matches",
                            worker_name
                        );
                        if !vm_manager.is_block_hash_matching(&vm.get_current_block_hash()) {
                            debug!(
                                "[{}] Block hash does not match, updating reference state",
                                worker_name
                            );
                            vm.update(vm_manager.get_reference_state())
                                .unwrap();

                            debug!(
                                "[{}] Reference state updated successfully to {}",
                                worker_name,
                                hex::encode(vm.get_current_block_hash().as_bytes())
                            );
                            return false; // Exit closure to restart mining loop
                        } else {
                            debug!(
                                "[{}] Block hash matches, not updating reference state",
                                worker_name
                            );
                        }
                        thread::yield_now();
                    }

                    if vm.get_current_height() != last_block_height {
                        blocks_processed += 1;
                        last_block_height = vm.get_current_height();
                    }

                    let mut input = [0u8; 64];
                    let block_hash = vm.get_current_block_hash();
                    input[..32].copy_from_slice(block_hash.as_bytes());
                    current_nonce.to_little_endian(&mut input[32..64]);

                    let hash_bytes = match vm.vm.calculate_hash(&input) {
                        Ok(hash) => hash,
                        Err(e) => {
                            error!("[{}] Failed to calculate hash: {}", worker_name, e);
                            return false;
                        }
                    };
                    let hash = H256::from_slice(&hash_bytes);
                    hashes_computed += 1;

                    if vm.check_hash(&hash) {
                        info!(
                            "[{}] Found solution! nonce={}, block hash={}, hash={}",
                            worker_name,
                            current_nonce,
                            hex::encode(vm.get_current_block_hash().as_bytes()),
                            hex::encode(hash)
                        );
                        let solution = ProofOfWorkSolution { nonce: current_nonce };
                        if let Err(e) = solution_sender.send((solution, vm.get_current_height())) {
                            warn!("[{}] Failed to send solution: {}", worker_name, e);
                        }
                        // Wait for new block
                        loop {
                            thread::sleep(Duration::from_millis(50));
                            if !vm_manager.is_block_hash_matching(&vm.get_current_block_hash()) {
                                debug!("[{}] New block detected after solution, resuming mining", worker_name);
                                vm.update(vm_manager.get_reference_state()).unwrap();
                                return false; // Exit closure to restart mining loop
                            } else {
                                trace!("[{}] Waiting for new block", worker_name);
                            }
                        }
                    }

                    current_nonce = current_nonce.overflowing_add(U256::from(1)).0;
                }
                false
            });

            if let Err(e) = result {
                error!("[{}] VM error: {}", worker_name, e);
                thread::sleep(Duration::from_secs(1));
                continue;
            }

            thread::yield_now();
        }
    }

    fn spawn_solution_handler(
        solution_rx: mpsc::Receiver<(ProofOfWorkSolution, u64)>,
        stratum_tx: broadcast::Sender<(ProofOfWorkSolution, u64)>,
        worker_name: String,
    ) {
        thread::spawn(move || {
            while let Ok((solution, solution_height)) = solution_rx.recv() {
                // Get current height from thread-local VM
                // let current_height = THREAD_VM.with(|vm| {
                //     vm.borrow()
                //         .as_ref()
                //         .map(|vm| vm.get_current_height())
                //         .unwrap_or(0)
                // });

                // // Skip stale solutions
                // if solution_height < current_height {
                //     debug!(
                //         "[{}] Skipping stale solution for block {}, current height: {}",
                //         worker_name, solution_height, current_height
                //     );
                //     continue;
                // }

                // Skip future solutions (shouldn't happen, but better be safe)
                // if solution_height > current_height {
                //     warn!(
                //         "[{}] Got solution for future block {} while at height {}",
                //         worker_name, solution_height, current_height
                //     );
                //     continue;
                // }

                // Forward valid solutions to stratum
                if let Err(e) = stratum_tx.send((solution, solution_height)) {
                    warn!(
                        "[{}] Failed to send solution to stratum: {}",
                        worker_name, e
                    );
                }
            }
        });
    }

    pub fn calculate_nonce_range(
        thread_id: usize, num_threads: usize,
    ) -> (U256, U256) {
        let nonce_range = U256::MAX / num_threads;
        let start_nonce = nonce_range * thread_id;
        let end_nonce = if thread_id == num_threads - 1 {
            U256::MAX
        } else {
            start_nonce + nonce_range
        };
        (start_nonce, end_nonce)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex::FromHex;
    use hex_literal::hex;
    use mazzecore::pow;
    use std::time::{Duration, Instant};

    // #[test]
    // fn test_mining_state_transitions() {
    //     // Create a single atomic state to track real transitions
    //     let atomic_state = AtomicProblemState::default();
    //     let initial_gen = atomic_state.get_generation();

    //     // Update with first state
    //     let state1 = ProblemState::new(
    //         1,
    //         H256::from([1u8; 32]), // Deterministic hash
    //         U256::from(1000000),
    //     );
    //     atomic_state.update(state1);
    //     let gen1 = atomic_state.get_generation();

    //     // Update with second state
    //     let state2 = ProblemState::new(
    //         2,
    //         H256::from([2u8; 32]), // Deterministic hash
    //         U256::from(1000000),
    //     );
    //     atomic_state.update(state2);
    //     let gen2 = atomic_state.get_generation();

    //     // Test generations increase with updates
    //     assert!(
    //         gen1 > initial_gen,
    //         "Generation should increase after first update"
    //     );
    //     assert!(
    //         gen2 > gen1,
    //         "Generation should increase after second update"
    //     );

    //     // Verify current state matches last update
    //     let (height, hash, boundary) = atomic_state.get_problem_details();
    //     assert_eq!(height, 2, "Height should match last update");
    //     assert_eq!(
    //         hash,
    //         H256::from([2u8; 32]),
    //         "Hash should match last update"
    //     );
    //     assert_eq!(
    //         boundary,
    //         U256::from(1000000),
    //         "Boundary should match last update"
    //     );
    // }

    #[test]
    fn test_boundary_comparison() {
        let boundary = hex!(
            "1222220000000000000000000000000000000000000000000000000000000000"
        );
        let hash = hex!(
            "9111110000000000000000000000000000000000000000000000000000000000"
        );

        let state = ProblemState::new(
            0,
            H256::zero(),
            U256::from_big_endian(&boundary),
        );

        let atomic_state = AtomicProblemState::new(
            0,
            H256::zero(),
            U256::from_big_endian(&boundary),
        );

        // This should print false because 0x91... > 0x12...
        println!("Hash: {}", hex::encode(&hash));
        println!("Boundary: {}", hex::encode(&boundary));
        println!(
            "Is hash <= boundary? {}",
            atomic_state.check_hash_simd(&H256::from_slice(&hash))
        );

        // Convert to U256 for direct comparison
        let hash_int = U256::from_big_endian(&hash);
        let boundary_int = U256::from_big_endian(&boundary);
        // println!("Direct comparison: {}", hash_int <= boundary_int);

        assert!(
            hash_int > boundary_int,
            "0x91... should be greater than 0x12..."
        );
        assert!(
            !atomic_state.check_hash_simd(&H256::from_slice(&hash)),
            "SIMD comparison should return false for hash > boundary"
        );
    }

    // #[test]
    // fn test_concurrent_state_transitions() {
    //     let atomic_state = Arc::new(AtomicProblemState::default());
    //     let thread_count = 4;
    //     let iterations = 100; // Reduced for clarity
    //     let mut handles = vec![];

    //     println!("Starting concurrent state transition test");
    //     let start = Instant::now();

    //     // Spawn checker threads
    //     for thread_id in 0..thread_count {
    //         let state = Arc::clone(&atomic_state);
    //         handles.push(thread::spawn(move || {
    //             let mut current_generation = 0;
    //             let mut transitions = 0;

    //             println!(
    //                 "Thread {} started checking for state changes",
    //                 thread_id
    //             );

    //             for i in 0..iterations {
    //                 let new_generation = state.get_generation();
    //                 if current_generation != new_generation {
    //                     println!(
    //                         "Thread {} detected change at iteration {}",
    //                         thread_id, i
    //                     );
    //                     current_generation = new_generation;
    //                     transitions += 1;
    //                 }
    //                 thread::sleep(Duration::from_millis(1)); // Small sleep to reduce CPU usage
    //             }

    //             println!(
    //                 "Thread {} finished with {} transitions",
    //                 thread_id, transitions
    //             );
    //             transitions
    //         }));
    //     }

    //     // Ensure threads have started
    //     thread::sleep(Duration::from_millis(50));

    //     // println!("Starting state updates");
    //     // Update shared state a few times
    //     for i in 1..=5 {
    //         let new_state = ProblemState::new(
    //             i,
    //             H256::from([i as u8; 32]), // Deterministic hash based on i
    //             U256::from(1000000),
    //         );
    //         // println!("Updating state to block height {}", i);
    //         atomic_state.update(new_state);
    //         thread::sleep(Duration::from_millis(50)); // Give more time for detection
    //     }

    //     let transitions: Vec<usize> =
    //         handles.into_iter().map(|h| h.join().unwrap()).collect();

    //     let total_transitions: usize = transitions.iter().sum();

    //     // println!("Test completed in {:?}", start.elapsed());
    //     // println!("Transitions per thread: {:?}", transitions);
    //     // println!("Total transitions: {}", total_transitions);

    //     // We expect each thread to see at least one change
    //     assert!(
    //         total_transitions >= thread_count,
    //         "Each thread should detect at least one state transition. \
    //          Expected at least {} total transitions, got {} \
    //          (transitions per thread: {:?})",
    //         thread_count,
    //         total_transitions,
    //         transitions
    //     );
    // }

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

    // #[test]
    // fn test_concurrent_mining() {
    //     let (mut miner, _rx) = Miner::new_legacy(4, 1);

    //     // Create and submit a problem
    //     let problem = ProofOfWorkProblem::new_from_boundary(
    //         1,
    //         H256::random(),
    //         U256::from(1000000),
    //     );

    //     miner.mine(&problem);

    //     // Let it mine for a bit
    //     thread::sleep(Duration::from_secs(1));

    //     // Submit a new problem
    //     let new_problem = ProofOfWorkProblem::new_from_boundary(
    //         2,
    //         H256::random(),
    //         U256::from(1000000),
    //     );

    //     miner.mine(&new_problem);

    //     // Verify all threads picked up the new problem
    //     thread::sleep(Duration::from_millis(100));

    //     let (height, _, _) = miner.atomic_state.get_problem_details();
    //     assert_eq!(height, 2, "All threads should be mining the new problem");
    // }

    // #[test]
    // fn test_single_mining_thread() {
    //     // Setup basic components
    //     let atomic_state = Arc::new(AtomicProblemState::default());
    //     let (solution_tx, solution_rx) = mpsc::channel();
    //     let core_id = core_affinity::get_core_ids().unwrap()[0];
    //     let diff = U256::from(4);
    //     let boundary = pow::difficulty_to_boundary(&diff);

    //     // Spawn the mining thread first
    //     let atomic_state_clone = Arc::clone(&atomic_state);
    //     let thread_handle = thread::spawn(move || {
    //         // println!("Mining thread started");
    //         Miner::run_mining_thread_legacy(
    //             0,
    //             core_id,
    //             "test-worker".to_string(),
    //             1,
    //             solution_tx,
    //             atomic_state_clone,
    //         )
    //     });

    //     // Give thread time to initialize
    //     thread::sleep(Duration::from_secs(2));

    //     // Now create and send the problem
    //     let problem =
    //         ProofOfWorkProblem::new_from_boundary(1, H256::random(), boundary);

    //     println!("Sending problem:");
    //     println!("  Height: {}", problem.block_height);
    //     println!("  Hash: {}", hex::encode(problem.block_hash.as_bytes()));
    //     println!("  Boundary: {}", problem.boundary);

    //     // Update atomic state with our problem
    //     atomic_state.update(ProblemState::from(&problem));
    //     atomic_state.update(ProblemState::from(
    //         &ProofOfWorkProblem::new_from_boundary(2, H256::random(), boundary),
    //     ));

    //     // Check for solutions
    //     let timeout = Duration::from_secs(5);
    //     let start = Instant::now();
    //     let mut solutions_found = 0;

    //     while start.elapsed() < timeout {
    //         match solution_rx.try_recv() {
    //             Ok((solution, height)) => {
    //                 println!(
    //                     "Found solution: nonce={}, height={}",
    //                     solution.nonce, height
    //                 );
    //                 solutions_found += 1;
    //             }
    //             Err(mpsc::TryRecvError::Empty) => {
    //                 thread::sleep(Duration::from_millis(100));
    //                 println!("Still mining... {:?}", start.elapsed());
    //             }
    //             Err(mpsc::TryRecvError::Disconnected) => {
    //                 panic!("Mining thread disconnected unexpectedly");
    //             }
    //         }
    //     }

    //     assert!(
    //         solutions_found > 0,
    //         "No solution found within timeout period"
    //     );
    //     println!("Found {} solutions", solutions_found);
    // }
}
