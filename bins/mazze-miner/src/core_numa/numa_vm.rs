use log::{debug, info, warn};
use mazze_types::{H256, U256};
use mazzecore::pow::ProofOfWorkProblem;
use randomx_rs::{RandomXCache, RandomXDataset, RandomXFlag, RandomXVM};
use std::cell::RefCell;
use std::{str::FromStr, sync::Arc};

use super::{NumaError, NumaTopology, ThreadAssignment};



pub struct ThreadLocalVM {
    vm: RandomXVM,
    current_block_hash: H256,
    current_height: u64,
}

impl ThreadLocalVM {
    pub fn new(node_id: usize) -> Result<Self, NumaError> {
        info!("Creating new thread-local VM for NUMA node {}", node_id);

        let topology = NumaTopology::detect()?;
        topology.bind_thread_to_node(node_id)?;

        let flags = RandomXFlag::get_recommended_flags();

        let GENESIS_BLOCK_HASH: H256 = H256::from_str(
            "ef6e5a0dd08b7c8be526c5d6ce7d2fcf8e4dd2449d690af4023f4ea989fd2a4e",
        )
        .expect("Invalid genesis hash");
        // Initialize with genesis block
        let cache = RandomXCache::new(flags, GENESIS_BLOCK_HASH.as_bytes())
            .map_err(|e| {
                warn!("Failed to create RandomX cache: {}", e);
                NumaError::RandomXError(e)
            })?;

        let vm = RandomXVM::new(flags, Some(cache), None)
            .map_err(NumaError::RandomXError)?;

        Ok(Self {
            vm,
            current_block_hash: GENESIS_BLOCK_HASH,
            current_height: 0,
        })
    }

    pub fn update_if_needed(
        &mut self, problem: &ProofOfWorkProblem,
    ) -> Result<(), NumaError> {
        if self.current_block_hash != problem.block_hash {
            debug!("Updating VM for new block hash: {}", problem.block_hash);

            let cache = RandomXCache::new(
                RandomXFlag::get_recommended_flags(), //TODO: use FULL_MEM flag too and create dataset
                problem.block_hash.as_bytes(),
            )
            .map_err(NumaError::RandomXError)?;

            self.vm
                .reinit_cache(cache)
                .map_err(NumaError::RandomXError)?;

            self.current_block_hash = problem.block_hash;
            self.current_height = problem.block_height;
        }
        Ok(())
    }

    pub fn calculate_hash(&self, nonce: U256, block_hash: &H256) -> H256 {
        let mut input = [0u8; 64];
        input[..32].copy_from_slice(block_hash.as_bytes());
        nonce.to_little_endian(&mut input[32..64]);

        let hash = self
            .vm
            .calculate_hash(&input)
            .expect("Failed to calculate hash");

        H256::from_slice(&hash)
    }

    pub fn get_current_block_hash(&self) -> H256 {
        self.current_block_hash
    }

    pub fn get_current_height(&self) -> u64 {
        self.current_height
    }

    pub fn check_hash(&self, hash: &H256, block_hash: &H256) -> Option<bool> {
        // Return None if block hash changed
        if block_hash != &self.current_block_hash {
            return None;
        }

        // TODO: Add actual boundary check here
        // For now, just compare against difficulty 4
        Some(true) // temporary
    }
}

thread_local! {
    pub static THREAD_VM: RefCell<Option<ThreadLocalVM>> = RefCell::new(None);
}

pub struct NewNumaVMManager {
    pub topology: NumaTopology,
}

impl NewNumaVMManager {
    pub fn new() -> Result<Self, NumaError> {
        Ok(Self {
            topology: NumaTopology::detect()?,
        })
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
        THREAD_VM.with(|vm| {
            if let Some(vm) = vm.borrow_mut().as_mut() {
                vm.update_if_needed(problem)?;
            }
            Ok(())
        })
    }
}
