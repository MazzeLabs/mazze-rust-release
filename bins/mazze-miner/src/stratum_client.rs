use futures::{SinkExt, StreamExt};
use log::{debug, error, info, trace, warn};
use mazze_types::U256;
use mazzecore::pow::{ProofOfWorkProblem, ProofOfWorkSolution};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tokio_util::codec::{Framed, LinesCodec};

use crate::miner::Miner;

pub struct StratumClient {
    framed: Framed<TcpStream, LinesCodec>,
    miner: Miner,
    current_job: Option<ProofOfWorkProblem>,
    stratum_secret: String,
    solution_receiver:
        tokio::sync::broadcast::Receiver<(ProofOfWorkSolution, u64)>,
}

impl StratumClient {
    pub async fn connect(
        addr: &str, stratum_secret: &str, miner: Miner,
        solution_receiver: tokio::sync::broadcast::Receiver<(
            ProofOfWorkSolution,
            u64,
        )>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        info!("Attempting to connect to {}", addr);
        let stream = TcpStream::connect(addr).await?;
        info!("Connected successfully to {}", addr);
        let framed = Framed::new(stream, LinesCodec::new());
        Ok(StratumClient {
            framed,
            miner,
            current_job: None,
            stratum_secret: stratum_secret.to_string(),
            solution_receiver,
        })
    }

    async fn subscribe(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Sending subscription request");
        let request = json!({
            "id": self.miner.worker_id,
            "method": "mining.subscribe",
            // TODO: investigate why self.stratum_secret is not working
            // (same config file used in miner and node too)
            // "params": ["999", self.stratum_secret]
            "params": ["999", "test"]
        });
        let request_json = serde_json::to_string(&request)?;
        trace!("Subscription request JSON: {}", request_json);
        self.framed.send(request_json).await?;
        info!("Subscription request sent");

        match self.receive_message().await? {
            Some(message) => {
                let value: Value = serde_json::from_str(&message)?;
                if let Some(result) = value.get("result") {
                    if result.as_bool() == Some(true) {
                        info!("Subscribed successfully");
                        Ok(())
                    } else {
                        warn!("Subscription failed");
                        Err("Subscription failed: {}".into())
                    }
                } else {
                    error!("Invalid subscription response");
                    Err("Invalid subscription response".into())
                }
            }
            None => {
                error!("No response received for subscription");
                Err("No response received for subscription".into())
            }
        }
    }

    async fn handle_job_notification(
        &mut self, params: &[Value],
    ) -> Result<(), Box<dyn std::error::Error>> {
        match self.miner.parse_job(params) {
            Ok(problem) => {
                self.current_job = Some(problem.clone());
                // self.miner.mine(&problem);
                Ok(())
            }
            Err(e) => {
                error!("Failed to parse job: {}", e);
                Err(e.into())
            }
        }
    }

    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.subscribe().await?;
        let mut solution_receiver = self.solution_receiver.resubscribe();

        loop {
            tokio::select! {
                message_result = self.receive_message() => {
                    match message_result? {
                        Some(message) => {
                            debug!("Received message: {}", message);
                            let value: Value = serde_json::from_str(&message)?;

                            if let Some(method) = value.get("method").and_then(Value::as_str) {
                                match method {
                                    "mining.notify" => {
                                        if let Some(params) = value.get("params").and_then(Value::as_array) {
                                            self.handle_job_notification(params).await?;
                                        }
                                    }
                                    _ => debug!("Received unknown method: {}", method),
                                }
                            } else if let Some(result) = value.get("result") {
                                debug!("Received result: {:?}", result);
                            } else {
                                debug!("Received unknown message: {}", message);
                            }
                        }
                        None => {
                            info!("Server closed the connection");
                            break;
                        }
                    }
                }
                Ok((solution, block_height)) = solution_receiver.recv() => {
                    info!("Received solution: {:?}, block_height: {}", solution.nonce, block_height);
                    if let Some(problem) = &self.current_job {
                        if block_height == problem.block_height {
                            self.submit_share(&solution).await?;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn receive_message(
        &mut self,
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        match timeout(Duration::from_secs(30), self.framed.next()).await {
            Ok(Some(line_result)) => Ok(Some(line_result?)),
            Ok(None) => Ok(None),
            Err(_) => Err("Timeout waiting for message".into()),
        }
    }

    async fn submit_share(
        &mut self, solution: &ProofOfWorkSolution,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(problem) = &self.current_job {
            info!("Submitting share for job: {}", problem.block_height);
            let request = json!({
                "id": format!("{}-{}", solution.nonce, problem.block_height),
                "method": "mining.submit",
                "params": [
                    self.miner.worker_name, // Worker names
                    problem.block_height.to_string(), // Job ID (assuming block height is used as job ID)
                    format!("0x{:x}", solution.nonce), // Nonce
                    format!("0x{:x}", problem.block_hash), // PoW Hash
                ]
            });
            let request_json = serde_json::to_string(&request)?;
            trace!("Submit share request JSON: {}", request_json);
            self.framed.send(request_json).await?;
            info!("Share submission sent: worker_id={}, job_id={}, nonce={}, pow_hash={}",
                self.miner.worker_name, problem.block_height, solution.nonce, problem.block_hash);
        } else {
            warn!("Attempted to submit share without a current job");
        }
        Ok(())
    }
}
