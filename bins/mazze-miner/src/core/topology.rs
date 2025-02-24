use hwlocality::cpu::binding::CpuBindingFlags;
use hwlocality::cpu::cpuset::CpuSet;
use hwlocality::object::types::ObjectType;
use hwlocality::Topology;
use log::{debug, info, warn};

use super::NumaError;

pub struct NumaTopology {
    topology: Topology,
}

impl NumaTopology {
    pub fn detect() -> Result<Self, NumaError> {
        info!("Detecting NUMA topology...");
        let topology = Topology::new().map_err(|e| {
            warn!("Failed to create topology: {}", e);
            NumaError::TopologyError(e.to_string())
        })?;

        debug!("NUMA topology detected successfully");
        Ok(Self { topology })
    }

    pub fn get_nodes(&self) -> Vec<usize> {
        let nodes = self
            .topology
            .objects_with_type(ObjectType::NUMANode)
            .map(|node| node.os_index().unwrap_or_default())
            .collect::<Vec<_>>();

        info!("Found {} NUMA nodes", nodes.len());
        debug!("NUMA nodes: {:?}", nodes);
        nodes
    }

    pub fn get_cores_for_node(
        &self, node_id: usize,
    ) -> Result<Vec<usize>, NumaError> {
        self.topology
            .objects_with_type(ObjectType::NUMANode)
            .find(|node| node.os_index().unwrap_or_default() == node_id)
            .and_then(|node| node.cpuset())
            .map(|cpuset| {
                cpuset.iter_set().map(|core_id| core_id.into()).collect()
            })
            .ok_or_else(|| NumaError::TopologyError("Node not found".into()))
    }

    pub fn bind_thread_to_node(&self, node_id: usize) -> Result<(), NumaError> {
        debug!("Attempting to bind thread to NUMA node {}", node_id);

        let node = self
            .topology
            .objects_with_type(ObjectType::NUMANode)
            .find(|node| node.os_index().unwrap_or_default() == node_id)
            .ok_or_else(|| {
                warn!("NUMA node {} not found", node_id);
                NumaError::TopologyError("Node not found".into())
            })?;

        if let Some(cpuset) = node.cpuset() {
            let mut owned_cpuset = CpuSet::new();
            let cpu_ids: Vec<_> = cpuset.iter_set().collect();
            debug!("Binding thread to CPUs: {:?}", cpu_ids);

            for cpu_id in cpu_ids {
                owned_cpuset.set(cpu_id);
            }

            self.topology
                .bind_cpu(&owned_cpuset, CpuBindingFlags::THREAD)
                .map_err(|e| {
                    warn!(
                        "Failed to bind thread to NUMA node {}: {}",
                        node_id, e
                    );
                    NumaError::ThreadBindError(e.to_string())
                })?;

            info!("Successfully bound thread to NUMA node {}", node_id);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topology_detection() {
        let topology = NumaTopology::detect();
        assert!(topology.is_ok(), "Should be able to detect topology");
    }

    #[test]
    fn test_get_nodes() {
        let topology = NumaTopology::detect().unwrap();
        let nodes = topology.get_nodes();
        assert!(!nodes.is_empty(), "Should find at least one NUMA node");
    }

    #[test]
    fn test_get_cores_for_node() {
        let topology = NumaTopology::detect().unwrap();
        let nodes = topology.get_nodes();

        for node_id in nodes {
            let cores = topology.get_cores_for_node(node_id);
            assert!(cores.is_ok(), "Should get cores for node {}", node_id);
            assert!(
                !cores.unwrap().is_empty(),
                "Node {} should have cores",
                node_id
            );
        }
    }

    #[test]
    fn test_invalid_node() {
        let topology = NumaTopology::detect().unwrap();
        let invalid_node = usize::MAX;
        let result = topology.get_cores_for_node(invalid_node);
        assert!(result.is_err(), "Should fail for invalid node");
    }

    #[test]
    fn test_thread_binding() {
        let topology = NumaTopology::detect().unwrap();
        let nodes = topology.get_nodes();

        if let Some(&first_node) = nodes.first() {
            let result = topology.bind_thread_to_node(first_node);
            assert!(result.is_ok(), "Should be able to bind to first node");
        }
    }
}

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
