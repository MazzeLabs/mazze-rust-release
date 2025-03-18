use log::{debug, info, warn};
use mazze_types::{H256, U256};
use mazzecore::pow::ProofOfWorkProblem;
use rust_randomx::{Context as RandomXContext, Hasher};
use std::cell::RefCell;
use std::{str::FromStr, sync::Arc};

use super::{NumaError, NumaTopology, ThreadAssignment};
use crate::core::{AtomicProblemState, ProblemState};

pub struct ThreadLocalVM {
    pub hasher: Hasher,
    problem_state: AtomicProblemState,
}

impl ThreadLocalVM {
    pub fn new(
        node_id: usize, ctx: Arc<RandomXContext>,
    ) -> Result<Self, NumaError> {
        info!("Creating new thread-local VM for NUMA node {}", node_id);

        let topology = NumaTopology::detect()?;
        topology.bind_thread_to_node(node_id)?;

        let hasher = Hasher::new(ctx);

        // Initialize with genesis state
        let problem_state = AtomicProblemState::default();
        debug!("Initialized problem state with default block");

        info!(
            "Thread-local VM initialization complete for NUMA node {}",
            node_id
        );

        Ok(Self { hasher, problem_state })
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
    context: Arc<RandomXContext>,
}

impl NewNumaVMManager {
    pub fn new() -> Result<Self, NumaError> {
        // TODO: init with new seed hash
        info!("Initializing RandomX context");
        let temp_seed_hash = [0u8; 32];
        let context = Arc::new(RandomXContext::new(&temp_seed_hash, true));
        info!("RandomX context initialized");

        Ok(Self {
            topology: NumaTopology::detect()?,
            reference_state: AtomicProblemState::default(),
            context,
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
                *vm_ref = Some(ThreadLocalVM::new(
                    assignment.node_id,
                    self.context.clone(),
                )?);
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
