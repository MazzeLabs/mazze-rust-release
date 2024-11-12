pub struct NumaInfo {
    node_count: usize,
    current_node: NodeId,
}

impl NumaInfo {
    pub fn detect() -> Option<Self> {
        if !numa::is_available() {
            return None;
        }
        Some(Self {
            node_count: numa::nodes().count(),
            current_node: numa::current_node(),
        })
    }
}

pub struct ThreadAssignment {
    thread_id: usize,
    node_id: NodeId,
    core_id: usize,
}

pub struct NumaThreadManager {
    assignments: Vec<ThreadAssignment>,
    topology: NumaTopology,
}

impl NumaThreadManager {
    pub fn new(requested_threads: usize) -> Result<Self, NumaError> {
        let topology = NumaTopology::detect()?;
        let assignments = Self::distribute_threads(&topology, requested_threads)?;
        
        Ok(Self {
            assignments,
            topology,
        })
    }

    pub fn assign_thread(&self, thread_id: usize) -> Result<ThreadAssignment, NumaError> {
        self.assignments.iter()
            .find(|a| a.thread_id == thread_id)
            .cloned()
            .ok_or(NumaError::ThreadAssignmentFailed)
    }

    fn distribute_threads(
        topology: &NumaTopology,
        requested_threads: usize
    ) -> Result<Vec<ThreadAssignment>, NumaError> {
        let mut assignments = Vec::new();
        let nodes = topology.get_nodes();
        
        // Distribute threads evenly across NUMA nodes
        for thread_id in 0..requested_threads {
            let node_id = nodes[thread_id % nodes.len()];
            let cores = topology.get_cores_for_node(node_id)?;
            let core_id = cores[thread_id / nodes.len() % cores.len()];
            
            assignments.push(ThreadAssignment {
                thread_id,
                node_id,
                core_id,
            });
        }
        
        Ok(assignments)
    }
}