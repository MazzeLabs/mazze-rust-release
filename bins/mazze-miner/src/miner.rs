use log::{info, trace};
use mazze_types::{H256, U256};
use mazzecore::pow::{
    boundary_to_difficulty, PowComputer, ProofOfWorkProblem,
    ProofOfWorkSolution,
};
use serde_json::Value;
use std::str::FromStr;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

struct MiningState {
    current_problem: Option<ProofOfWorkProblem>,
    is_solved: bool,
    solution_sender: Option<mpsc::Sender<ProofOfWorkSolution>>,
}

#[derive(Clone)]
pub struct Miner {
    pub worker_id: usize,
    pub worker_name: String,
    pow_computer: Arc<PowComputer>,
    num_threads: usize,
    state: Arc<Mutex<MiningState>>,
}

impl Miner {
    pub fn new(num_threads: usize, worker_id: usize) -> Self {
        let state = Arc::new(Mutex::new(MiningState {
            current_problem: None,
            is_solved: false,
            solution_sender: None,
        }));

        let miner = Miner {
            worker_id,
            worker_name: format!("worker-{}", worker_id),
            pow_computer: Arc::new(PowComputer::new()),
            num_threads,
            state,
        };

        miner.spawn_mining_threads();
        miner
    }

    fn spawn_mining_threads(&self) {
        let partition_size = U256::MAX / U256::from(self.num_threads);

        for i in 0..self.num_threads {
            let state = Arc::clone(&self.state);
            let pow = Arc::clone(&self.pow_computer);
            let start_nonce = U256::from(i) * partition_size;
            let nonce_range = partition_size;
            let worker_name = self.worker_name.clone();

            info!(
                "[{}] Spawning mining thread {} with nonce range start: {}",
                worker_name, i, start_nonce
            );

            thread::spawn(move || {
                info!("[{}] Mining thread {} started", worker_name, i);

                loop {
                    let (problem, sender) = {
                        let state_guard = state.lock().unwrap();
                        if state_guard.is_solved {
                            trace!(
                                "[{}] Thread {}: Problem solved, yielding",
                                worker_name,
                                i
                            );
                            thread::yield_now();
                            continue;
                        }
                        (
                            state_guard.current_problem.clone(),
                            state_guard.solution_sender.clone(),
                        )
                    };

                    match (problem, sender) {
                        (Some(problem), Some(sender)) => {
                            trace!(
                                "[{}] Thread {}: Mining with nonce start {}",
                                worker_name,
                                i,
                                start_nonce
                            );
                            if let Some(solution) = pow.mine_range(
                                &problem,
                                start_nonce,
                                Duration::from_secs(1),
                            ) {
                                let mut state_guard = state.lock().unwrap();
                                if !state_guard.is_solved {
                                    info!("[{}] Thread {}: Found solution with nonce {}", worker_name, i, solution.nonce);
                                    state_guard.is_solved = true;
                                    let _ = sender.send(solution);
                                }
                            }
                        }
                        _ => {
                            trace!(
                                "[{}] Thread {}: No problem to solve, yielding",
                                worker_name,
                                i
                            );
                            thread::yield_now();
                        }
                    }
                }
            });
        }
    }

    pub fn mine(
        &self, problem: &ProofOfWorkProblem, timeout: Duration,
    ) -> Option<ProofOfWorkSolution> {
        let (tx, rx) = mpsc::channel();

        {
            let mut state = self.state.lock().unwrap();
            state.current_problem = Some(problem.clone());
            state.is_solved = false;
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
