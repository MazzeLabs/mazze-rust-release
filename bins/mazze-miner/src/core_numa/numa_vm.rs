use log::{debug, info, warn};
use mazze_types::{H256, U256};
use mazzecore::pow::ProofOfWorkProblem;
use parking_lot::RwLock;
use randomx_rs::{RandomXCache, RandomXDataset, RandomXFlag, RandomXVM};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;
use std::{collections::HashMap, sync::atomic::AtomicBool};

use crate::core::{AtomicProblemState, BatchHasher, ProblemState};

use super::{topology::NumaTopology, NumaError};

pub struct NewNumaVMManager {
    vms: Vec<Arc<NumaVM>>, // One VM per NUMA node
    topology: NumaTopology,
}

impl NewNumaVMManager {
    pub fn new() -> Result<Self, NumaError> {
        let topology = NumaTopology::detect()?;
        let nodes = topology.get_nodes();

        let vms = nodes
            .iter()
            .map(|&node_id| -> Result<Arc<NumaVM>, NumaError> {
                Ok(Arc::new(NumaVM::new(node_id)?))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self { vms, topology })
    }

    pub fn get_vm(&self, node_id: usize) -> Arc<NumaVM> {
        self.vms[node_id].clone()
    }

    pub fn assign_thread(
        &self, thread_id: usize,
    ) -> Result<ThreadAssignment, NumaError> {
        let num_nodes = self.vms.len();
        let node_id = thread_id % num_nodes;
        let cores = self.topology.get_cores_for_node(node_id)?;
        let core_id = (thread_id / num_nodes) % cores.len();

        Ok(ThreadAssignment::new(thread_id, node_id, core_id))
    }

    pub fn update_all_vms(
        &self, problem: ProofOfWorkProblem,
    ) -> Result<(), NumaError> {
        for vm in &self.vms {
            vm.update_if_needed(problem.clone())?;
        }
        Ok(())
    }
}

// One per thread
#[repr(align(64))]
pub struct NumaMiningState {
    vm: RandomXVM,
    problem: AtomicProblemState,
    state_id: u64,
}

impl NumaMiningState {
    pub fn get_hash_batch(
        &self, hasher: &mut BatchHasher, current_nonce: U256,
    ) -> Vec<H256> {
        hasher.compute_hash_batch(
            &self.vm,
            current_nonce,
            &self.problem.get_block_hash(),
        )
    }

    pub fn get_hash(&self, nonce: U256) -> H256 {
        // Fixed-size array instead of Vec
        let mut input = [0u8; 64];

        // Copy block hash
        input[..32].copy_from_slice(self.problem.get_block_hash().as_bytes());

        // Set nonce
        nonce.to_little_endian(&mut input[32..64]);

        // Calculate single hash
        let hash = self
            .vm
            .calculate_hash(&input)
            .expect("Failed to calculate hash");

        H256::from_slice(&hash)
    }

    pub fn get_state_id(&self) -> u64 {
        self.state_id
    }

    pub fn get_problem_block_height(&self) -> u64 {
        self.problem.get_block_height()
    }
}

// One per NUMA node
#[repr(align(64))]
pub struct NumaVM {
    active_state: AtomicPtr<NumaMiningState>,
    padding: [u8; 64], // Prevent false sharing
    standby_state: AtomicPtr<NumaMiningState>,
    node_id: usize,
    flags: RandomXFlag,
}

impl NumaVM {
    pub fn new(node_id: usize) -> Result<Self, NumaError> {
        info!("Creating new NUMA VM for node {}", node_id);

        let topology = NumaTopology::detect()?;
        topology.bind_thread_to_node(node_id)?;

        let flags = RandomXFlag::get_recommended_flags();
        if Self::check_node_memory(node_id)? {
            info!("SKIPPED: Enabling full memory mode for node {}", node_id);
            // flags |= RandomXFlag::FLAG_FULL_MEM;
        }

        // Initialize first VM
        let cache = RandomXCache::new(flags, &[0; 32]).map_err(|e| {
            warn!("Failed to create RandomX cache: {}", e);
            NumaError::RandomXError(e)
        })?;

        let dataset = if flags.contains(RandomXFlag::FLAG_FULL_MEM) {
            Some(RandomXDataset::new(flags, cache.clone(), 0).map_err(|e| {
                warn!("Failed to create RandomX dataset: {}", e);
                NumaError::RandomXError(e)
            })?)
        } else {
            None
        };

        let active_vm =
            RandomXVM::new(flags, Some(cache.clone()), dataset.clone())
                .map_err(NumaError::RandomXError)?;
        let standby_vm = RandomXVM::new(flags, Some(cache), dataset)
            .map_err(NumaError::RandomXError)?;

        let active_state = Box::into_raw(Box::new(NumaMiningState {
            vm: active_vm,
            problem: AtomicProblemState::default(),
            state_id: 1,
        }));

        let standby_state = Box::into_raw(Box::new(NumaMiningState {
            vm: standby_vm,
            problem: AtomicProblemState::default(),
            state_id: 2,
        }));

        Ok(Self {
            active_state: AtomicPtr::new(active_state),
            padding: [0u8; 64],
            standby_state: AtomicPtr::new(standby_state),
            node_id,
            flags,
        })
    }

    pub fn calculate_nonce_range(
        &self, thread_id: usize, num_threads: usize,
    ) -> (U256, U256) {
        let active = unsafe { &*self.active_state.load(Ordering::Acquire) };
        active.problem.calculate_nonce_range(thread_id, num_threads)
    }

    pub fn check_hash(
        &self, hash: &H256, current_state_id: u64,
    ) -> Option<bool> {
        let active = unsafe { &*self.active_state.load(Ordering::Acquire) };
        if active.state_id != current_state_id {
            return None; // Signal state change
        }
        Some(active.problem.check_hash_simd(hash))
    }

    pub fn get_active_state(&self) -> &NumaMiningState {
        unsafe { &*self.active_state.load(Ordering::Acquire) }
    }

    pub fn update_if_needed(
        &self, problem: ProofOfWorkProblem,
    ) -> Result<(), NumaError> {
        let active_ptr = self.active_state.load(Ordering::Acquire);
        let active = unsafe { &*active_ptr };

        if active.problem.get_block_hash() != problem.block_hash {
            debug!(
                "Starting state update: active_ptr={:p}, active_state_id={}",
                active_ptr, active.state_id
            );

            let standby_ptr = self.standby_state.load(Ordering::Acquire);
            let standby = unsafe { &mut *standby_ptr };
            debug!(
                "Loaded standby: standby_ptr={:p}, standby_state_id={}",
                standby_ptr, standby.state_id
            );

            // Update standby state
            let new_cache =
                RandomXCache::new(self.flags, &problem.block_hash.0)
                    .map_err(NumaError::RandomXError)?;
            standby
                .vm
                .reinit_cache(new_cache)
                .map_err(NumaError::RandomXError)?;

            standby.problem.update(ProblemState::from(&problem));
            debug!("Updated standby state with new problem");

            // Ensure all updates are complete before swap
            std::sync::atomic::fence(Ordering::Release);

            // Single atomic swap to exchange the pointers
            let old_active =
                self.active_state.swap(standby_ptr, Ordering::AcqRel);
            self.standby_state.store(old_active, Ordering::Release);

            debug!(
                "Completed state swap: new_active_ptr={:p}, new_standby_ptr={:p}",
                standby_ptr,
                old_active
            );
        }
        Ok(())
    }

    pub fn check_node_memory(node_id: usize) -> Result<bool, NumaError> {
        debug!("Checking available memory for node {}", node_id);

        #[cfg(target_os = "linux")]
        {
            let meminfo =
                std::fs::read_to_string("/proc/meminfo").map_err(|e| {
                    warn!("Failed to read meminfo: {}", e);
                    NumaError::MemoryError("Failed to read meminfo".into())
                })?;

            let available = meminfo
                .lines()
                .find(|line| line.starts_with("MemAvailable:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|kb_str| kb_str.parse::<u64>().ok())
                .ok_or_else(|| {
                    warn!("Failed to parse memory info");
                    NumaError::MemoryError("Failed to parse memory info".into())
                })?;

            let available_gb = available / (1024 * 1024);
            info!("Node {} has {}GB available memory", node_id, available_gb);
            Ok(available_gb >= 3)
        }

        #[cfg(not(target_os = "linux"))]
        {
            warn!("Memory check not supported on non-Linux systems");
            Ok(false)
        }
    }
}

impl Drop for NumaVM {
    fn drop(&mut self) {
        // Clean up the raw pointers
        unsafe {
            let _ = Box::from_raw(self.active_state.load(Ordering::Acquire));
            let _ = Box::from_raw(self.standby_state.load(Ordering::Acquire));
        }
    }
}

/*
 *
 *
 * OLD CODE:
 *
 *
 */

#[derive(Clone, Debug)]
pub struct ThreadAssignment {
    pub thread_id: usize,
    pub node_id: usize,
    pub core_id: usize,
}

impl ThreadAssignment {
    pub fn new(thread_id: usize, node_id: usize, core_id: usize) -> Self {
        debug!(
            "Creating thread assignment: thread={}, node={}, core={}",
            thread_id, node_id, core_id
        );
        Self {
            thread_id,
            node_id,
            core_id,
        }
    }
}
