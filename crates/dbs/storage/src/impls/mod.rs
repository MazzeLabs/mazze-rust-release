// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

#[macro_use]
pub(super) mod merkle_patricia_trie;
pub(super) mod delta_mpt;
pub(super) mod node_merkle_proof;
pub(super) mod proof_merger;
pub(super) mod recording_storage;
pub(super) mod replicated_state;
pub(super) mod single_mpt_state;
pub(super) mod snapshot_sync;
pub(super) mod state;
pub(super) mod state_manager;
pub(super) mod state_proof;
pub(super) mod storage_db;
pub(super) mod storage_manager;

pub(super) use mazze_db_errors::storage as errors;

pub mod defaults {
    pub use super::delta_mpt::DEFAULT_NODE_MAP_SIZE;
    // By default do not check for data-integrity for snapshot mpt for
    // production runs.
    pub const DEFAULT_DEBUG_SNAPSHOT_CHECKER_THREADS: u16 = 0;
    pub const DEFAULT_DELTA_MPTS_CACHE_RECENT_LFU_FACTOR: f64 =
        DeltaMptsNodeMemoryManager::R_LFU_FACTOR;
    pub const DEFAULT_DELTA_MPTS_CACHE_SIZE: u32 =
        DeltaMptsNodeMemoryManager::MAX_CACHED_TRIE_NODES_DISK_HYBRID;
    pub const DEFAULT_DELTA_MPTS_CACHE_START_SIZE: u32 =
        DeltaMptsNodeMemoryManager::START_CAPACITY;
    pub const DEFAULT_DELTA_MPTS_SLAB_IDLE_SIZE: u32 =
        DeltaMptsNodeMemoryManager::MAX_DIRTY_AND_TEMPORARY_TRIE_NODES;
    // Bumped 4 -> 8: the prefetcher warms the state cache for an epoch's tx
    // senders/targets and execution blocks until it finishes. More threads =
    // more concurrent state reads served (esp. under MDBX), shortening the
    // synchronous prefetch stall before each epoch executes.
    pub const DEFAULT_EXECUTION_PREFETCH_THREADS: usize = 8;
    /// Limit the number of open snapshots to set an upper limit on open files
    /// in Storage subsystem.
    pub const DEFAULT_MAX_OPEN_SNAPSHOTS: u16 = 10;
    pub const MAX_CACHED_TRIE_NODES_R_LFU_COUNTER: u32 =
        DeltaMptsNodeMemoryManager::MAX_CACHED_TRIE_NODES_R_LFU_COUNTER;

    /// The max number of opened MPT databases at the same time.
    /// Accessing a state currently involves both the intermediate MPT and delta
    /// MPT, so setting this to 4 allows to access two states at the same
    /// time.
    pub const DEFAULT_MAX_OPEN_MPT: u32 = 4;
    /// Default MDBX map size in MB to avoid hard failures at large state sizes.
    pub const DEFAULT_MDBX_MAP_SIZE_MB: u64 = 65_536;

    // -------------------- snapshot MDBX geometry (Phase 5c) --------------------
    //
    // The snapshot env is opened growth-enabled per design doc §2.3.1
    // (the hot env's pinned-64GB behaviour is out of scope for 5c).
    // Full-fast retention (≤ 2 eras) sits comfortably in a few GB;
    // archive retention (100+ generations) wants operators to raise
    // `snapshot_mdbx_max_mb`. Choose defaults that (a) leave room for
    // the observed ~35 MB/snapshot × 100 gens × 3× (KV + MPT + slack)
    // ≈ 10 GB archive floor, plus (b) a safety margin so operators
    // hit the capacity alert (see `sample_snapshot_mdbx_capacity`)
    // well before `MDBX_MAP_FULL`.
    /// Initial map size for the snapshot env. Small enough that a
    /// fresh node doesn't over-commit disk — MDBX allocates on
    /// demand within the [`initial`, `max`] range.
    pub const DEFAULT_SNAPSHOT_MDBX_INITIAL_MB: u64 = 1_024;
    /// Ceiling the snapshot env can grow toward. Archives should
    /// override this in `hydra.toml`.
    pub const DEFAULT_SNAPSHOT_MDBX_MAX_MB: u64 = 32_768;
    /// Growth step. Larger → fewer file-growth syscalls at write
    /// bursts; smaller → less slack between grows.
    pub const DEFAULT_SNAPSHOT_MDBX_GROWTH_STEP_MB: u64 = 2_048;

    use super::delta_mpt::node_memory_manager::DeltaMptsNodeMemoryManager;
}
