use log::{debug, info, warn};
use mazze_types::{H256, U256};
use mazzecore::pow::ProofOfWorkProblem;
use randomx_rs::{RandomXCache, RandomXDataset, RandomXFlag, RandomXVM};
use std::cell::RefCell;
use std::{str::FromStr, sync::Arc};

use super::{NumaError, NumaTopology, ThreadAssignment};
use crate::core::{AtomicProblemState, ProblemState};

pub struct ThreadLocalVM {
    pub vm: RandomXVM,
    problem_state: AtomicProblemState,
}

impl ThreadLocalVM {
    pub fn new(node_id: usize) -> Result<Self, NumaError> {
        info!("Creating new thread-local VM for NUMA node {}", node_id);

        let topology = NumaTopology::detect()?;
        topology.bind_thread_to_node(node_id)?;

        // TODO: init with new seed hash
        let temp_seed_hash: H256 = H256::from_str(
            "ef6e5a0dd08b7c8be526c5d6ce7d2fcf8e4dd2449d690af4023f4ea989fd2a4e",
        )
        .expect("Invalid seed hash");

        let mut flags = RandomXFlag::get_recommended_flags();
        flags |= RandomXFlag::FLAG_FULL_MEM;
        debug!("Initializing RandomX with flags: {:?}", flags);

        // Initialize with genesis block
        info!(
            "Creating RandomX cache with genesis block: {}",
            temp_seed_hash
        );
        let cache = RandomXCache::new(flags, temp_seed_hash.as_bytes())
            .map_err(|e| {
                warn!("Failed to create RandomX cache: {}", e);
                NumaError::RandomXError(e)
            })?;
        debug!("RandomX cache created successfully");

        info!("Creating RandomX dataset...");
        let dataset =
            RandomXDataset::new(flags, cache.clone(), 0).map_err(|e| {
                warn!("Failed to create RandomX dataset: {}", e);
                NumaError::RandomXError(e)
            })?;
        info!("RandomX dataset created successfully");

        info!("Creating RandomX VM...");
        let vm = RandomXVM::new(flags, Some(cache), Some(dataset))
            .map_err(NumaError::RandomXError)?;
        info!("RandomX VM created successfully");

        // Initialize with genesis state
        let problem_state = AtomicProblemState::new(
            0, // Initial height
            temp_seed_hash,
            U256::from(4), // Initial difficulty
        );
        debug!("Initialized problem state with genesis block");

        info!(
            "Thread-local VM initialization complete for NUMA node {}",
            node_id
        );

        Ok(Self { vm, problem_state })
    }

    pub fn get_current_block_hash(&self) -> H256 {
        self.problem_state.get_block_hash()
    }

    pub fn get_current_height(&self) -> u64 {
        self.problem_state.get_block_height()
    }

    pub fn check_hash(&self, hash: &H256) -> bool {
        self.problem_state.check_hash_simd(hash)
    }

    pub fn update(
        &mut self, reference_state: ProblemState,
    ) -> Result<(), NumaError> {
        self.problem_state.update(reference_state);
        Ok(())
    }
}

thread_local! {
    pub static THREAD_VM: RefCell<Option<ThreadLocalVM>> = RefCell::new(None);
}

pub struct NewNumaVMManager {
    pub topology: NumaTopology,
    reference_state: AtomicProblemState,
}

impl NewNumaVMManager {
    pub fn new() -> Result<Self, NumaError> {
        Ok(Self {
            topology: NumaTopology::detect()?,
            reference_state: AtomicProblemState::default(),
        })
    }

    pub fn is_block_hash_matching(&self, block_hash: &H256) -> bool {
        self.reference_state.matches(block_hash)
    }

    pub fn get_reference_state(&self) -> ProblemState {
        ProblemState::from(&self.reference_state)
    }

    pub fn with_vm<F, R>(
        &self, assignment: &ThreadAssignment, f: F,
    ) -> Result<R, NumaError>
    where
        F: FnOnce(&mut ThreadLocalVM) -> R,
    {
        THREAD_VM.with(|vm| {
            let mut vm_ref = vm.borrow_mut();
            if vm_ref.is_none() {
                *vm_ref = Some(ThreadLocalVM::new(assignment.node_id)?);
            }
            Ok(f(vm_ref.as_mut().unwrap()))
        })
    }

    pub fn update_if_needed(
        &self, problem: &ProofOfWorkProblem,
    ) -> Result<(), NumaError> {
        debug!(
            "Updating reference state to new block hash: {}",
            problem.block_hash
        );
        self.reference_state.update(ProblemState::from(problem));
        info!("Reference state updated successfully");

        Ok(())
    }
}
