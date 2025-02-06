use std::{sync::atomic::{AtomicU64, Ordering}, time::{Duration, Instant}};

use log::info;

#[allow(dead_code)]
pub struct MiningMetrics {
    total_hashes: AtomicU64,
    total_blocks: AtomicU64,
    last_hash_report: Instant,
    last_block_report: Instant,
}

#[allow(dead_code)]
impl MiningMetrics {
    pub fn new() -> Self {
        Self {
            total_hashes: AtomicU64::new(0),
            total_blocks: AtomicU64::new(0),
            last_hash_report: Instant::now(),
            last_block_report: Instant::now(),
        }
    }

    pub fn add_hashes(&self, count: u64) {
        self.total_hashes.fetch_add(count, Ordering::Relaxed);
    }

    pub fn new_block(&self) {
        self.total_blocks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn report_metrics(&mut self) {
        let now = Instant::now();

        // Report hash rate every 5 seconds
        let hash_elapsed = now.duration_since(self.last_hash_report);
        if hash_elapsed >= Duration::from_secs(5) {
            let hashes = self.total_hashes.swap(0, Ordering::Relaxed);
            let hash_rate = hashes as f64 / hash_elapsed.as_secs_f64();
            info!("Global hash rate: {:.2} H/s", hash_rate);
            self.last_hash_report = now;
        }

        // Report block rate every 10 seconds
        let block_elapsed = now.duration_since(self.last_block_report);
        if block_elapsed >= Duration::from_secs(10) {
            let blocks = self.total_blocks.swap(0, Ordering::Relaxed);
            let block_rate = blocks as f64 / block_elapsed.as_secs_f64();
            info!("Block processing rate: {:.2} blocks/s", block_rate);
            self.last_block_report = now;
        }
    }
}
