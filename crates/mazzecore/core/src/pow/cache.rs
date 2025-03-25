#![allow(dead_code)]
#![allow(unused_imports)]

use parking_lot::Mutex;
use rust_randomx::{Context as RandomXContext, Hasher};

use super::{
    compute::Light,
    keccak::{keccak_512, H256},
    seed_compute::SeedHashCompute,
    shared::{
        get_cache_size, Node, NODE_BYTES, POW_CACHE_ROUNDS, POW_STAGE_LENGTH,
    },
};
use std::str::FromStr;

use std::{collections::HashMap, slice, sync::Arc};

pub type Cache = Vec<Node>;

use crossbeam_deque::{Steal, Stealer, Worker};
use parking_lot::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

const INITIAL_VMS_PER_STAGE: usize = 4;

pub struct RandomXCacheBuilder {
    global_queue: Worker<Arc<Hasher>>,
    stealers: Vec<Stealer<Arc<Hasher>>>,
    context: RwLock<Arc<RandomXContext>>,
    current_seed_hash: RwLock<H256>,
}

pub struct VMHandle {
    vm: Option<Arc<Hasher>>,
    cache_builder: Arc<RandomXCacheBuilder>,
}

impl RandomXCacheBuilder {
    pub fn new() -> Arc<Self> {
        let global_queue = Worker::new_fifo();
        let stealers = vec![global_queue.stealer()];

        let temp_seed_hash = H256::default();
        let context = Arc::new(RandomXContext::new(&temp_seed_hash, false));

        let builder = Arc::new(RandomXCacheBuilder {
            global_queue,
            stealers,
            context: RwLock::new(context),
            current_seed_hash: RwLock::new(temp_seed_hash),
        });

        // Initialize with some VMs
        for _ in 0..INITIAL_VMS_PER_STAGE {
            let vm = builder.initialize_new_vm();
            builder.global_queue.push(vm);
        }

        builder
    }

    fn initialize_new_vm(&self) -> Arc<Hasher> {
        Arc::new(Hasher::new(self.context.read().clone()))
    }

    fn update_context(&self, seed_hash: &H256) {
        let mut current_hash = self.current_seed_hash.write();
        if *current_hash != *seed_hash {
            // Create new context with the new seed hash
            let new_context = Arc::new(RandomXContext::new(seed_hash, false));
            
            // Update the context with new one
            *self.context.write() = new_context;
            
            // Update seed hash
            *current_hash = *seed_hash;
            
            // Clear existing VMs as they're using the old context
            while let Some(_) = self.global_queue.pop() {}
            
            // Repopulate with new VMs
            for _ in 0..INITIAL_VMS_PER_STAGE {
                let vm = self.initialize_new_vm();
                self.global_queue.push(vm);
            }
        }
    }

    fn get_stage_key(&self, block_height: u64) -> Vec<u8> {
        let stage = block_height / 2048;
        format!("stage_{}", stage).into_bytes()
    }

    fn acquire_vm(&self) -> Arc<Hasher> {
        if let Some(vm) = self.global_queue.pop() {
            return vm;
        }

        for stealer in &self.stealers {
            match stealer.steal() {
                Steal::Success(vm) => return vm,
                Steal::Empty => continue,
                Steal::Retry => {
                    if let Steal::Success(vm) = stealer.steal() {
                        return vm;
                    }
                }
            }
        }

        self.initialize_new_vm()
    }

    fn return_vm_handler(&self, vm: Arc<Hasher>) {
        self.global_queue.push(vm);
    }

    pub fn get_vm_handler(self: &Arc<Self>, seed_hash: &H256) -> VMHandle {
        // Update context if seed hash changed
        self.update_context(seed_hash);

        VMHandle {
            vm: Some(self.acquire_vm()),
            cache_builder: Arc::clone(self),
        }
    }

    /*
       Hasher -> has update function that takes a context
       We're storing context locally, should we? No.
    */
}

impl VMHandle {
    pub fn get_vm(&self) -> &Hasher {
        self.vm.as_ref().expect("VM should exist").as_ref()
    }
}

impl Drop for VMHandle {
    fn drop(&mut self) {
        if let Some(vm) = self.vm.take() {
            self.cache_builder.return_vm_handler(vm);
        }
    }
}
