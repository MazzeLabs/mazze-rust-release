#![allow(dead_code)]
#![allow(unused_imports)]

use parking_lot::Mutex;
use rust_randomx::{Context as RandomXContext, Hasher};

use std::str::FromStr;

use std::{collections::HashMap, slice, sync::Arc};

use crossbeam_deque::{Steal, Stealer, Worker};
use parking_lot::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

const INITIAL_VMS_PER_STAGE: usize = 4;

pub type H256 = [u8; 32];

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
    pub fn new(seed_hash: H256) -> Arc<Self> {
        let global_queue = Worker::new_fifo();
        let stealers = vec![global_queue.stealer()];

        let context = Arc::new(RandomXContext::new(&seed_hash, false));

        let builder = Arc::new(RandomXCacheBuilder {
            global_queue,
            stealers,
            context: RwLock::new(context),
            current_seed_hash: RwLock::new(seed_hash),
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
        debug!("Updating RandomX Context for seed hash: {:?}", seed_hash);
        let mut current_hash = self.current_seed_hash.write();
        if *current_hash != *seed_hash {
            // Create new context with the new seed hash
            let new_context = Arc::new(RandomXContext::new(seed_hash, false));

            // Update the context with new one
            *self.context.write() = new_context;

            // Update seed hash
            *current_hash = *seed_hash;
        }
    }

    pub fn get_seed_hash(&self) -> H256 {
        self.current_seed_hash.read().clone()
    }

    fn acquire_vm(&self) -> Arc<Hasher> {
        let current_context = self.context.read().clone();
        let current_key = current_context.key();

        // Try to get a VM from our queue
        if let Some(vm) = self.global_queue.pop() {
            // Check if VM needs an update
            let vm_key = vm.context().key();
            if vm_key == current_key {
                return vm; // VM is up-to-date
            }

            // VM needs an update - clone it and update its context
            // Note: We need to clone because Arc prevents mutation
            let new_vm = Hasher::new(current_context);
            return Arc::new(new_vm);
        }

        // Try to steal from other workers
        for stealer in &self.stealers {
            match stealer.steal() {
                Steal::Success(vm) => {
                    let vm_key = vm.context().key();
                    if vm_key == current_key {
                        return vm;
                    }
                    let new_vm = Hasher::new(current_context.clone());
                    return Arc::new(new_vm);
                }
                Steal::Empty => continue,
                Steal::Retry => {
                    if let Steal::Success(vm) = stealer.steal() {
                        let vm_key = vm.context().key();
                        if vm_key == current_key {
                            return vm;
                        }
                        let new_vm = Hasher::new(current_context.clone());
                        return Arc::new(new_vm);
                    }
                }
            }
        }

        // No VM available, create a new one
        self.initialize_new_vm()
    }

    fn return_vm_handler(&self, vm: Arc<Hasher>) {
        // Optionally, we could check if VM's context is current before returning it
        let current_context = self.context.read().clone();
        let current_key = current_context.key();
        let vm_key = vm.context().key();

        if vm_key == current_key {
            // VM is current, return it to the queue
            self.global_queue.push(vm);
        }
        // If VM is outdated, let it be dropped
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
