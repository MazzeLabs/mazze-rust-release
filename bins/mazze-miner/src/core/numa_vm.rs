use log::{debug, info, warn};
use mazze_types::{H256, U256};
use mazzecore::pow::ProofOfWorkProblem;
use rust_randomx::{Context as RandomXContext, Hasher};
use std::cell::RefCell;
use std::{str::FromStr, sync::Arc, sync::RwLock};

use super::{NumaError, NumaTopology, ThreadAssignment};
use crate::core::{AtomicProblemState, ProblemState};

pub struct ThreadLocalVM {
    pub hasher: Hasher,
    problem_state: AtomicProblemState,
}

impl ThreadLocalVM {
    pub fn new(
        node_id: usize, ctx: Arc<RandomXContext>, topology: &NumaTopology,
    ) -> Result<Self, NumaError> {
        info!("Creating new thread-local VM for NUMA node {}", node_id);

        topology.bind_thread_to_node(node_id)?;

        let hasher = Hasher::new(ctx);

        // Initialize with genesis state
        let problem_state = AtomicProblemState::default();
        debug!("Initialized problem state with default block");

        info!(
            "Thread-local VM initialization complete for NUMA node {}",
            node_id
        );

        Ok(Self {
            hasher,
            problem_state,
        })
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
        &mut self, reference_state: ProblemState, context: Arc<RandomXContext>,
    ) -> Result<(), NumaError> {
        self.problem_state.update(reference_state);

        if self.hasher.context().key() != self.problem_state.get_seed_hash() {
            self.hasher.update(context);
        }

        Ok(())
    }
}

thread_local! {
    pub static THREAD_VM: RefCell<Option<ThreadLocalVM>> = RefCell::new(None);
}

pub struct VMManager {
    pub topology: NumaTopology,
    reference_state: AtomicProblemState,
    context: RwLock<Arc<RandomXContext>>,
}

impl VMManager {
    pub fn new() -> Result<Self, NumaError> {
        // TODO: init with new seed hash
        info!("Initializing RandomX context");
        // This is the genesis hash, we should receive a new hash on subscribe or delay VM creation
        let temp_seed_hash = [
            64, 150, 60, 66, 190, 75, 98, 194, 155, 219, 240, 243, 85, 138, 89,
            208, 98, 34, 241, 9, 35, 101, 195, 39, 166, 14, 116, 82, 106, 188,
            165, 14,
        ];
        let context =
            RwLock::new(Arc::new(RandomXContext::new(&temp_seed_hash, true)));
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

    pub fn get_context(&self) -> Arc<RandomXContext> {
        // Get a read lock and clone the Arc to avoid holding the lock
        self.context.read().unwrap().clone()
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
                    self.get_context(), // Using get_context() to access through RwLock
                    &self.topology,
                )?);
            }
            Ok(f(vm_ref.as_mut().unwrap()))
        })
    }

    pub fn update_if_needed(
        &self, problem: &ProofOfWorkProblem,
    ) -> Result<(), NumaError> {
        let problem_seed_hash = problem.seed_hash.as_bytes();
        if problem_seed_hash != self.reference_state.get_seed_hash() {
            // Update context using the RwLock for interior mutability
            let mut context_write = self.context.write().unwrap();
            *context_write =
                Arc::new(RandomXContext::new(problem_seed_hash, true));
            debug!("RandomX context updated with new seed hash");
        }

        debug!(
            "Updating reference state to new block hash: {}",
            problem.block_hash
        );
        self.reference_state.update(ProblemState::from(problem));
        info!("Reference state updated successfully");

        Ok(())
    }
}
