use log::{info, trace};
use mazze_types::{H256, U256};
use mazzecore::pow::{
    boundary_to_difficulty, ProofOfWorkProblem, ProofOfWorkSolution,
};
use randomx_rs::{RandomXCache, RandomXFlag, RandomXVM};
use serde_json::Value;
use std::str::FromStr;
use std::sync::{mpsc, Arc, RwLock};
use std::thread;
use std::time::Instant;

const CHECK_INTERVAL: u64 = 100; // Check for new problem every 100 nonces

struct MiningState {
    current_problem: Option<ProofOfWorkProblem>,
    solution_sender: Option<mpsc::Sender<ProofOfWorkSolution>>,
}

#[derive(Clone)]
pub struct Miner {
    pub worker_id: usize,
    pub worker_name: String,
    num_threads: usize,
    state: Arc<RwLock<MiningState>>,
}

impl Miner {
    pub fn new(num_threads: usize, worker_id: usize) -> Self {
        let state = Arc::new(RwLock::new(MiningState {
            current_problem: None,
            solution_sender: None,
        }));

        let miner = Miner {
            worker_id,
            worker_name: format!("worker-{}", worker_id),
            num_threads,
            state,
        };

        miner.spawn_mining_threads();
        miner
    }

    fn compute_hash(vm: &RandomXVM, nonce: &U256, block_hash: &H256) -> H256 {
        let mut input = [0u8; 64];
        for i in 0..32 {
            input[i] = block_hash[i];
        }
        nonce.to_little_endian(&mut input[32..64]);

        let hash = vm.calculate_hash(&input).expect("Failed to calculate hash");
        H256::from_slice(&hash)
    }

    fn calculate_nonce_range(
        thread_id: usize, num_threads: usize, boundary: &U256,
    ) -> (U256, U256) {
        // Calculate total search space based on boundary
        let total_space = U256::MAX / boundary;

        // Calculate range size for each thread
        let range_size = total_space / U256::from(num_threads);

        // Calculate start and end for this thread
        let start = U256::from(thread_id) * range_size * boundary;
        let end = if thread_id == num_threads - 1 {
            U256::MAX
        } else {
            (start + range_size * boundary) - U256::one()
        };

        (start, end)
    }

    fn spawn_mining_threads(&self) {
        for i in 0..self.num_threads {
            let state = Arc::clone(&self.state);
            let worker_name = self.worker_name.clone();
            let num_threads = self.num_threads;

            info!("[{}] Spawning mining thread {}", worker_name, i);

            thread::spawn(move || {
                info!("[{}] Mining thread {} started", worker_name, i);

                // Initialize RandomX VM for this thread
                let flags = RandomXFlag::get_recommended_flags();
                let vm = RandomXVM::new(flags, None, None)
                    .expect("Failed to create RandomX VM");

                let mut current_problem: Option<ProofOfWorkProblem> = None;

                loop {
                    // Read current problem state (no locks held during mining)
                    let (problem, sender) = {
                        let state_guard = state.read().unwrap();
                        (
                            state_guard.current_problem.clone(),
                            state_guard.solution_sender.clone(),
                        )
                    };

                    match (problem, sender) {
                        (Some(problem), Some(sender)) => {
                            // Check if problem changed
                            if current_problem.as_ref() != Some(&problem) {
                                current_problem = Some(problem.clone());
                                info!(
                                    "[{}] Thread {}: New problem received, block_height: {}",
                                    worker_name, i, problem.block_height
                                );
                            }

                            let (start_nonce, end_nonce) =
                                Self::calculate_nonce_range(
                                    i,
                                    num_threads,
                                    &problem.boundary,
                                );
                            let mut current_nonce = start_nonce;
                            while current_nonce < end_nonce {
                                // Check for new problem periodically
                                if current_nonce.low_u64() % CHECK_INTERVAL == 0
                                {
                                    let state_guard = state.read().unwrap();
                                    if state_guard.current_problem.as_ref()
                                        != Some(&problem)
                                    {
                                        break;
                                    }
                                }

                                let hash = Self::compute_hash(
                                    &vm,
                                    &current_nonce,
                                    &problem.block_hash,
                                );
                                let hash_u256 = U256::from(hash.as_bytes());

                                if hash_u256 <= problem.boundary {
                                    info!(
                                        "[{}] Thread {}: Found solution with nonce {}",
                                        worker_name, i, current_nonce
                                    );
                                    let solution = ProofOfWorkSolution {
                                        nonce: current_nonce,
                                    };
                                    let _ = sender.send(solution);
                                    break;
                                }

                                current_nonce += U256::one();
                            }
                        }
                        _ => {
                            thread::yield_now();
                        }
                    }
                }
            });
        }
    }

    pub fn mine(
        &self, problem: &ProofOfWorkProblem, timeout: std::time::Duration,
    ) -> Option<ProofOfWorkSolution> {
        let (tx, rx) = mpsc::channel();

        {
            let mut state = self.state.write().unwrap();
            state.current_problem = Some(problem.clone());
            state.solution_sender = Some(tx);
        }

        rx.recv_timeout(timeout).ok()
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
}
