#[derive(Debug)]
pub enum NumaError {
    TopologyError(String),
    ThreadBindError(String),
    RandomXError(randomx_rs::RandomXError),
    MemoryError(String),
    ThreadAssignmentFailed,
}

impl std::fmt::Display for NumaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NumaError::TopologyError(e) => write!(f, "Topology error: {}", e),
            NumaError::ThreadBindError(e) => {
                write!(f, "Thread binding error: {}", e)
            }
            NumaError::RandomXError(e) => write!(f, "RandomX error: {}", e),
            NumaError::MemoryError(e) => write!(f, "Memory error: {}", e),
            NumaError::ThreadAssignmentFailed => {
                write!(f, "Thread assignment failed")
            }
        }
    }
}
