mod atomic_state;
mod batch_hasher;
mod thread_vm;

pub use atomic_state::*;
pub use batch_hasher::*;
pub use thread_vm::*;

// Common traits
pub trait IntoChunks {
    fn into_chunks(self) -> [u64; 4];
}

// Common constants
pub const CHECK_INTERVAL: u64 = 2;
pub const BATCH_SIZE: usize = 8;
