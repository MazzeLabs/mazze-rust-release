use hwlocality::{object::types::ObjectType, Topology};
use log::{debug, info, warn};
use parking_lot::RwLock;
use randomx_rs::{RandomXCache, RandomXFlag, RandomXVM};
use std::collections::HashMap;
use std::sync::Arc;

use super::{topology::NumaTopology, NumaError, ThreadAssignment};

pub struct NumaAwareVM {
    vm: Arc<RwLock<RandomXVM>>,
    cache: Arc<RwLock<RandomXCache>>,
    node_id: usize,
    flags: RandomXFlag,
}

unsafe impl Send for NumaAwareVM {}
unsafe impl Sync for NumaAwareVM {}

impl NumaAwareVM {
    pub fn new(node_id: usize) -> Result<Self, NumaError> {
        info!("Creating new NUMA-aware VM for node {}", node_id);

        let topology = NumaTopology::detect()?;
        topology.bind_thread_to_node(node_id)?;

        let flags = Self::get_optimal_flags(node_id)?;
        debug!("Using RandomX flags: {:?}", flags);

        info!("Initializing RandomX cache for node {}", node_id);
        let cache = Arc::new(RwLock::new(
            RandomXCache::new(flags, &[0; 32]).map_err(|e| {
                warn!("Failed to create RandomX cache: {}", e);
                NumaError::RandomXError(e)
            })?,
        ));

        info!("Creating RandomX VM for node {}", node_id);
        let vm = Arc::new(RwLock::new(
            RandomXVM::new(flags, Some(cache.read().clone()), None).map_err(
                |e| {
                    warn!("Failed to create RandomX VM: {}", e);
                    NumaError::RandomXError(e)
                },
            )?,
        ));

        info!("Successfully created NUMA-aware VM for node {}", node_id);
        Ok(Self {
            vm,
            cache,
            node_id,
            flags,
        })
    }

    pub fn get_vm(&self) -> parking_lot::RwLockReadGuard<'_, RandomXVM> {
        self.vm.read()
    }

    fn get_optimal_flags(node_id: usize) -> Result<RandomXFlag, NumaError> {
        debug!("Getting optimal RandomX flags for node {}", node_id);
        let mut flags = RandomXFlag::get_recommended_flags();

        if Self::check_node_memory(node_id)? {
            info!("Enabling full memory mode for node {}", node_id);
            flags |= RandomXFlag::FLAG_FULL_MEM;
        } else {
            warn!("Insufficient memory for full mode on node {}", node_id);
        }

        Ok(flags)
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
        debug!("Updating VM on node {} with new block hash", self.node_id);

        let new_cache =
            RandomXCache::new(self.flags, block_hash).map_err(|e| {
                warn!("Failed to create new cache: {}", e);
                NumaError::RandomXError(e)
            })?;

        {
            let mut vm = self.vm.write();
            debug!("Reinitializing VM cache on node {}", self.node_id);
            vm.reinit_cache(new_cache.clone()).map_err(|e| {
                warn!("Failed to reinit VM cache: {}", e);
                NumaError::RandomXError(e)
            })?;
        }

        *self.cache.write() = new_cache;
        info!("Successfully updated VM on node {}", self.node_id);
        Ok(())
    }
}

pub struct NumaVMManager {
    vms: Vec<Arc<RwLock<NumaAwareVM>>>,
    topology: NumaTopology,
    active_threads: parking_lot::RwLock<HashMap<usize, ThreadAssignment>>,
}

impl NumaVMManager {
    pub fn new() -> Result<Self, NumaError> {
        let topology = NumaTopology::detect()?;
        let vms = Self::initialize_vms(&topology)?;

        Ok(Self {
            vms,
            topology,
            active_threads: parking_lot::RwLock::new(HashMap::new()),
        })
    }

    pub fn get_vm_read(
        &self, node_id: usize,
    ) -> parking_lot::RwLockReadGuard<'_, NumaAwareVM> {
        self.vms[node_id].read()
    }

    // For write access
    pub fn get_vm_write(
        &self, node_id: usize,
    ) -> parking_lot::RwLockWriteGuard<'_, NumaAwareVM> {
        self.vms[node_id].write()
    }

    fn initialize_vms(
        topology: &NumaTopology,
    ) -> Result<Vec<Arc<RwLock<NumaAwareVM>>>, NumaError> {
        let nodes = topology.get_nodes();
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
        &self, thread_id: usize, total_threads: usize,
    ) -> Result<ThreadAssignment, NumaError> {
        let mut active_threads = self.active_threads.write();

        // Check if thread is already assigned
        if let Some(assignment) = active_threads.get(&thread_id) {
            return Ok(assignment.clone());
        }

        // Calculate optimal node assignment
        let node_id = thread_id % self.vms.len();
        let cores = self.topology.get_cores_for_node(node_id)?;
        let core_id = (thread_id / self.vms.len()) % cores.len();

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
