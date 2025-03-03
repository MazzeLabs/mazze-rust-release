#![allow(dead_code)]
#![allow(unused_imports)]

use log::{error, info};
use mazzecore::pow::ProofOfWorkSolution;
use miner_config::MinerConfig;
use std::process;
use tokio;
use tokio::signal::ctrl_c;
use tokio::time::{sleep, Duration};

mod miner;
mod miner_config;
mod stratum_client;
mod core;

use miner::Miner;
use stratum_client::StratumClient;

async fn connect_with_retry(
    config: &MinerConfig, miner: Miner,
    solution_receiver: tokio::sync::broadcast::Receiver<(
        ProofOfWorkSolution,
        u64,
    )>,
) -> Result<StratumClient, Box<dyn std::error::Error>> {
    let initial_delay = Duration::from_secs(1);
    let max_delay = Duration::from_secs(60);
    let mut delay = initial_delay;

    loop {
        match StratumClient::connect(
            &config.stratum_address,
            &config.stratum_secret,
            miner.clone(),
            solution_receiver.resubscribe(),
        )
        .await
        {
            Ok(client) => {
                info!("Connected to server successfully");
                return Ok(client);
            }
            Err(e) => {
                error!(
                    "Failed to connect: {:?}. Retrying in {:?}...",
                    e, delay
                );
                sleep(delay).await;
                delay = std::cmp::min(delay * 2, max_delay);
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // env_logger::init();
    env_logger::builder()
        .format_timestamp_millis()
        .filter_module(
            "mazze_miner::core::atomic_state",
            log::LevelFilter::Debug,
        )
        .init();

    info!("Initializing Mazze Miner client...");

    let config = match MinerConfig::new() {
        Ok(config) => config,
        Err(e) => {
            error!("Failed to load configuration: {:?}", e);
            return Err(e);
        }
    };

    info!(
        "Starting Mazze Miner client with worker id {} and num_threads {}",
        config.worker_id, config.num_threads
    );

    // let (miner, solution_receiver) =
    //     Miner::new(config.num_threads, config.worker_id);

    // Create NUMA-aware miner instead of legacy miner
    let (miner, solution_receiver) =
        match Miner::new_numa(config.num_threads, config.worker_id) {
            Ok(result) => result,
            Err(e) => {
                error!("Failed to initialize NUMA-aware miner: {:?}", e);
                return Err("Failed to initialize NUMA-aware miner".into());
                // return Err(e.into());
            }
        };

    info!("Sleeping for 30 seconds to allow VMs to initialize...");
    tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;

    // Set up Ctrl+C handler
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move {
        if let Err(e) = ctrl_c().await {
            error!("Failed to listen for Ctrl+C: {:?}", e);
        }
        let _ = tx.send(()).await;
    });

    loop {
        match connect_with_retry(
            &config,
            miner.clone(),
            solution_receiver.resubscribe(),
        )
        .await
        {
            Ok(mut client) => {
                info!("Starting mining operation");
                tokio::select! {
                    result = client.run() => {
                        match result {
                            Ok(_) => info!("Mining operation completed successfully"),
                            Err(e) => error!("Error during client execution: {:?}", e),
                        }
                    }
                    _ = rx.recv() => {
                        info!("Received shutdown signal. Stopping mining operation.");
                        break;
                    }
                }
            }
            Err(e) => {
                error!("Failed to connect to the node: {:?}", e);
                process::exit(1);
            }
        }
    }

    info!("Shutting down Mazze Miner client...");
    Ok(())
}
