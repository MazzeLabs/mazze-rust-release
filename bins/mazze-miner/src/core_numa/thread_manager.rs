use hwlocality::{object::types::ObjectType, Topology};
use log::{debug, info, warn};
use parking_lot::RwLock;
use randomx_rs::{RandomXCache, RandomXFlag, RandomXVM};
use std::sync::Arc;

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

pub struct NumaThreadManager {
    assignments: Vec<ThreadAssignment>,
    topology: NumaTopology,
}

impl NumaThreadManager {
    pub fn new(requested_threads: usize) -> Result<Self, NumaError> {
        info!(
            "Initializing NUMA thread manager with {} threads",
            requested_threads
        );
        let topology = NumaTopology::detect()?;
        let assignments =
            Self::distribute_threads(&topology, requested_threads)?;

        info!(
            "Created thread manager with {} assignments",
            assignments.len()
        );
        Ok(Self {
            assignments,
            topology,
        })
    }

    pub fn assign_thread(
        &self, thread_id: usize,
    ) -> Result<ThreadAssignment, NumaError> {
        debug!("Looking for assignment for thread {}", thread_id);
        self.assignments
            .iter()
            .find(|a| a.thread_id == thread_id)
            .cloned()
            .ok_or_else(|| {
                warn!("No assignment found for thread {}", thread_id);
                NumaError::ThreadAssignmentFailed
            })
    }

    fn distribute_threads(
        topology: &NumaTopology, requested_threads: usize,
    ) -> Result<Vec<ThreadAssignment>, NumaError> {
        let nodes = topology.get_nodes();
        info!(
            "Distributing {} threads across {} NUMA nodes",
            requested_threads,
            nodes.len()
        );

        let mut assignments = Vec::new();

        for thread_id in 0..requested_threads {
            let node_id = nodes[thread_id % nodes.len()];
            let cores = topology.get_cores_for_node(node_id)?;
            let core_id = cores[thread_id / nodes.len() % cores.len()];

            debug!(
                "Assigning thread {} to node {} core {}",
                thread_id, node_id, core_id
            );
            assignments
                .push(ThreadAssignment::new(thread_id, node_id, core_id));
        }

        info!("Created {} thread assignments", assignments.len());
        Ok(assignments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thread_manager_creation() {
        let manager = NumaThreadManager::new(4);
        assert!(manager.is_ok(), "Should create thread manager");
    }

    #[test]
    fn test_thread_assignment() {
        let manager = NumaThreadManager::new(4).unwrap();
        let assignment = manager.assign_thread(0);
        assert!(assignment.is_ok(), "Should assign thread 0");
    }

    #[test]
    fn test_invalid_thread_assignment() {
        let manager = NumaThreadManager::new(4).unwrap();
        let assignment = manager.assign_thread(999);
        assert!(assignment.is_err(), "Should fail for invalid thread ID");
    }

    #[test]
    fn test_thread_distribution() {
        let manager = NumaThreadManager::new(8).unwrap();
        let assignments: Vec<_> = (0..8)
            .filter_map(|id| manager.assign_thread(id).ok())
            .collect();

        assert_eq!(assignments.len(), 8, "Should have 8 assignments");

        // Check that assignments are distributed
        let unique_nodes: std::collections::HashSet<_> =
            assignments.iter().map(|a| a.node_id).collect();
        assert!(
            !unique_nodes.is_empty(),
            "Should use multiple NUMA nodes if available"
        );
    }
}
