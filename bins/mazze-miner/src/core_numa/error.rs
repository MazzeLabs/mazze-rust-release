#[derive(Debug)]
pub enum NumaError {
    TopologyError(String),
    ThreadBindError(String),
    RandomXError(randomx_rs::RandomXError),
    MemoryError(String),
    ThreadAssignmentFailed
}