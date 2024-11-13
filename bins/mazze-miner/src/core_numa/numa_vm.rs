use log::{debug, info, warn};
use parking_lot::RwLock;
use randomx_rs::{RandomXCache, RandomXDataset, RandomXFlag, RandomXVM};
use std::sync::Arc;
use std::{collections::HashMap, sync::atomic::AtomicBool};

use super::{topology::NumaTopology, NumaError};

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

pub struct NumaAwareVM {
    vm: Arc<RwLock<RandomXVM>>,
    standby_vm: Arc<RwLock<RandomXVM>>,
    node_id: usize,
    flags: RandomXFlag,
    current_key: [u8; 32],
}

unsafe impl Send for NumaAwareVM {}
unsafe impl Sync for NumaAwareVM {}

impl NumaAwareVM {
    pub fn new(node_id: usize) -> Result<Self, NumaError> {
        info!("Creating new NUMA-aware VM for node {}", node_id);

        let topology = NumaTopology::detect()?;
        topology.bind_thread_to_node(node_id)?;

        let flags = RandomXFlag::get_recommended_flags();

        if Self::check_node_memory(node_id)? {
            info!("SKIPPED: Enabling full memory mode for node {}", node_id);
            // flags |= RandomXFlag::FLAG_FULL_MEM;
        } else {
            warn!(
                "Insufficient memory for full mode on node {}, defaulting",
                node_id
            );
        }

        info!("Initializing RandomX cache for node {}", node_id);
        let cache = Arc::new(RwLock::new(
            RandomXCache::new(flags, &[0; 32]).map_err(|e| {
                warn!("Failed to create RandomX cache: {}", e);
                NumaError::RandomXError(e)
            })?,
        ));

        let dataset = if flags.contains(RandomXFlag::FLAG_FULL_MEM) {
            info!("Creating RandomX dataset for node {}", node_id);
            Some(RandomXDataset::new(flags, cache.read().clone(), 0).map_err(
                |e| {
                    warn!("Failed to create RandomX dataset: {}", e);
                    NumaError::RandomXError(e)
                },
            )?)
        } else {
            None
        };

        info!("Creating RandomX VMs for node {}", node_id);
        let vm = Arc::new(RwLock::new(
            RandomXVM::new(flags, Some(cache.read().clone()), dataset.clone())
                .map_err(|e| {
                    warn!("Failed to create RandomX VM: {}", e);
                    NumaError::RandomXError(e)
                })?,
        ));

        let standby_vm = Arc::new(RwLock::new(
            RandomXVM::new(flags, Some(cache.read().clone()), dataset)
                .map_err(|e| {
                    warn!("Failed to create RandomX VM: {}", e);
                    NumaError::RandomXError(e)
                })?,
        ));

        info!("Successfully created NUMA-aware VM for node {}", node_id);
        Ok(Self {
            vm,
            standby_vm,
            node_id,
            flags,
            current_key: [0u8; 32],
        })
    }

    pub fn get_vm(&self) -> parking_lot::RwLockReadGuard<'_, RandomXVM> {
        debug!(
            "Thread {:?} attempting to acquire inner VM read lock for node {}",
            std::thread::current().id(),
            self.node_id
        );
        let guard = self.vm.read();
        debug!(
            "Thread {:?} acquired inner VM read lock for node {}",
            std::thread::current().id(),
            self.node_id
        );
        guard
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

    pub fn update_if_needed(
        &mut self, block_hash: &[u8; 32],
    ) -> Result<(), NumaError> {
        if self.current_key != *block_hash {
            info!("Updating RandomX VM for node {} with new key", self.node_id);

            // Create new cache
            let new_cache = RandomXCache::new(self.flags, block_hash)
                .map_err(NumaError::RandomXError)?;

            // Update standby VM in a separate scope
            {
                debug!(
                    "Thread {:?} attempting to acquire standby VM write lock for node {}",
                    std::thread::current().id(),
                    self.node_id
                );
                let mut standby = self.standby_vm.write();
                debug!(
                    "Thread {:?} acquired standby VM write lock for node {}",
                    std::thread::current().id(),
                    self.node_id
                );
                standby
                    .reinit_cache(new_cache)
                    .map_err(NumaError::RandomXError)?;
            } // standby lock is released here

            // Now safe to swap
            std::mem::swap(&mut self.vm, &mut self.standby_vm);
            self.current_key = *block_hash;

            info!("Successfully updated RandomX VM for node {}", self.node_id);
        }
        Ok(())
    }
}

pub struct NumaVMManager {
    vms: Vec<Arc<RwLock<NumaAwareVM>>>,
    topology: NumaTopology,
    active_threads: parking_lot::RwLock<HashMap<usize, ThreadAssignment>>,
    is_updating: Arc<AtomicBool>,
}

impl NumaVMManager {
    pub fn new() -> Result<Self, NumaError> {
        let topology = NumaTopology::detect()?;
        let nodes = topology.get_nodes();
        let vms = Self::initialize_vms(&topology, &nodes)?;

        Ok(Self {
            vms,
            topology,
            active_threads: parking_lot::RwLock::new(HashMap::new()),
            is_updating: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn is_updating(&self) -> bool {
        self.is_updating.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn get_vm_read(
        &self, node_id: usize,
    ) -> parking_lot::RwLockReadGuard<'_, NumaAwareVM> {
        debug!(
            "Thread {:?} attempting to acquire VM read lock for node {}",
            std::thread::current().id(),
            node_id
        );
        let guard = self.vms[node_id].read();
        debug!(
            "Thread {:?} acquired VM read lock for node {}",
            std::thread::current().id(),
            node_id
        );
        guard
    }

    // For write access
    pub fn get_vm_write(
        &self, node_id: usize,
    ) -> parking_lot::RwLockWriteGuard<'_, NumaAwareVM> {
        self.vms[node_id].write()
    }

    fn initialize_vms(
        topology: &NumaTopology, nodes: &[usize],
    ) -> Result<Vec<Arc<RwLock<NumaAwareVM>>>, NumaError> {
        info!("Initializing VMs for {} NUMA nodes", nodes.len());

        nodes
            .iter()
            .map(|&node_id| {
                info!("Creating VM for NUMA node {}", node_id);
                let vm = NumaAwareVM::new(node_id)?;
                Ok(Arc::new(RwLock::new(vm)))
            })
            .collect()
    }

    pub fn assign_thread(
        &self, thread_id: usize,
    ) -> Result<ThreadAssignment, NumaError> {
        let mut active_threads = self.active_threads.write();
        let num_nodes = self.vms.len();
        let node_id = thread_id % num_nodes;
        let cores = self.topology.get_cores_for_node(node_id)?;
        let core_id = (thread_id / num_nodes) % cores.len();

        let assignment = ThreadAssignment::new(thread_id, node_id, core_id);
        active_threads.insert(thread_id, assignment.clone());

        Ok(assignment)
    }

    pub fn update_all_vms(
        &self, block_hash: &[u8; 32],
    ) -> Result<(), NumaError> {
        for (node_id, vm) in self.vms.iter().enumerate() {
            info!("Updating VM for NUMA node {}", node_id);
            vm.write().update_if_needed(block_hash)?;
        }
        Ok(())
    }

    pub fn cleanup_thread(&self, thread_id: usize) {
        let mut active_threads = self.active_threads.write();
        active_threads.remove(&thread_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_numa_vm_creation() {
        let topology = NumaTopology::detect().unwrap();
        let nodes = topology.get_nodes();

        if !nodes.is_empty() {
            let vm = NumaAwareVM::new(nodes[0]);
            assert!(vm.is_ok(), "Failed to create NUMA VM");
        }
    }

    #[test]
    fn test_vm_manager() {
        let manager = NumaVMManager::new();
        assert!(manager.is_ok(), "Failed to create VM manager");

        if let Ok(manager) = manager {
            assert!(manager.vms.len() > 0, "Should have at least one node");
        }
    }

    #[test]
    fn test_vm_update() {
        if let Ok(manager) = NumaVMManager::new() {
            let result = manager.update_all_vms(&[0; 32]);
            assert!(result.is_ok(), "Should update all VMs successfully");
        }
    }
}
