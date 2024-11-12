#[derive(Debug, thiserror::Error)]
pub enum NumaError {
    #[error("NUMA is not available on this system")]
    NumaNotAvailable,
    #[error("Failed to detect NUMA topology")]
    TopologyDetectionFailed,
    #[error("Failed to set thread affinity")]
    ThreadAffinityFailed,
    #[error("RandomX error: {0}")]
    RandomX(#[from] randomx_rs::RandomXError),
}