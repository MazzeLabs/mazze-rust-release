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

        for thread_id in 0..self.num_threads {
            let worker_name = self.worker_name.clone();
            let num_threads = self.num_threads;
            let solution_sender = self.solution_sender.clone();
            let core_id = core_ids[thread_id % core_count];
            let atomic_state = Arc::clone(&self.atomic_state);

            info!(
                "[{}] Spawning mining thread {} on core {}",
                worker_name, thread_id, core_id.id
            );

            thread::spawn(move || {
                // Pin thread to specific core
                core_affinity::set_for_current(core_id);

                info!(
                    "[{}] Mining thread {} started on core {}",
                    worker_name, thread_id, core_id.id
                );

                let flags = RandomXFlag::get_recommended_flags();
                let mut vm = ThreadLocalVM::new(flags, &H256::zero());
                let mut hasher = BatchHasher::new();

                // Create a snapshot of the current problem
                let current_state = AtomicProblemState::default();

                loop {
                    // Get current problem details atomically
                    let (height, block_hash, boundary) =
                        atomic_state.get_problem_details();

                    // Compare using our new traits
                    if current_state != *atomic_state {
                        info!(
                            "[{}] Thread {}: New problem received, block_height: {}",
                            worker_name, thread_id, height
                        );

                        // Update current state
                        current_state.update(&atomic_state);
                        vm.update_if_needed(&block_hash);

                        let (start_nonce, end_nonce) = current_state
                            .calculate_nonce_range(thread_id, num_threads);
                        let mut current_nonce = start_nonce;

                        while current_nonce < end_nonce {
                            // Check if problem changed using trait comparison
                            if current_nonce.low_u64() % CHECK_INTERVAL == 0
                                && current_state != *atomic_state
                            {
                                break;
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
                                            worker_name, thread_id, e
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
    use std::time::Instant;

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
        let mut current_state = AtomicProblemState::default();
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
}
