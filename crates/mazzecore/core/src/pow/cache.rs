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

    context: Arc<RandomXContext>,
    current_stage: AtomicU64,
}

pub struct VMHandle {
    vm: Option<Arc<Hasher>>,
    cache_builder: Arc<RandomXCacheBuilder>,
}

impl RandomXCacheBuilder {
    pub fn new() -> Arc<Self> {
        let global_queue = Worker::new_fifo();
        let stealers = vec![global_queue.stealer()];

        let temp_seed_hash = [0u8; 32];
        let context = Arc::new(RandomXContext::new(&temp_seed_hash, false));

        let builder = RandomXCacheBuilder {
            global_queue,
            stealers,
            current_stage: AtomicU64::new(0),
            context,
        };

        // Initialize with some VMs
        for _ in 0..INITIAL_VMS_PER_STAGE {
            let vm = builder.initialize_new_vm(0);
            builder.global_queue.push(vm);
        }

        Arc::new(builder)
    }

    fn initialize_new_vm(&self, _block_height: u64) -> Arc<Hasher> {
        Arc::new(Hasher::new(self.context.clone()))
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

        let current_stage = self.current_stage.load(Ordering::Acquire);
        self.initialize_new_vm(current_stage)
    }

    fn return_vm_handler(&self, vm: Arc<Hasher>) {
        self.global_queue.push(vm);
    }

    pub fn get_vm_handler(
        self: &Arc<Self>, seed_hash: &H256,
    ) -> VMHandle {
        VMHandle {
            vm: Some(self.acquire_vm()),
            cache_builder: Arc::clone(self),
        }
    }
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

// Obsolete implementation of the cache builder - octopus specific.
#[derive(Clone)]
pub struct CacheBuilder {
    seedhash: Arc<Mutex<SeedHashCompute>>,
    caches: Arc<Mutex<HashMap<u64, Arc<Cache>>>>,
}

impl CacheBuilder {
    pub fn new() -> Self {
        CacheBuilder {
            seedhash: Arc::new(Mutex::new(SeedHashCompute::default())),
            caches: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn light(&self, block_height: u64) -> Light {
        Light::new_with_builder(self, block_height)
    }

    fn block_height_to_ident(&self, block_height: u64) -> H256 {
        self.seedhash.lock().hash_block_height(block_height)
    }

    #[allow(dead_code)]
    fn stage_to_ident(&self, stage: u64) -> H256 {
        self.seedhash.lock().hash_stage(stage)
    }

    pub fn new_cache(&self, block_height: u64) -> Arc<Cache> {
        let stage = block_height / POW_STAGE_LENGTH;

        let mut caches = self.caches.lock();
        if let Some(cache) = caches.get(&stage) {
            return cache.clone();
        }

        let ident = self.block_height_to_ident(block_height);
        let cache_size = get_cache_size(block_height);

        // We use `debug_assert` since it is impossible for `get_cache_size` to
        // return an unaligned value with the current implementation. If
        // the implementation changes, CI will catch it.
        debug_assert!(cache_size % NODE_BYTES == 0, "Unaligned cache size");
        let num_nodes = cache_size / NODE_BYTES;

        let cache = Arc::new(make_memory_cache(num_nodes, &ident));
        caches.insert(stage, cache.clone());

        cache
    }
}

fn make_memory_cache(num_nodes: usize, ident: &H256) -> Cache {
    let mut nodes: Vec<Node> = Vec::with_capacity(num_nodes);
    // Use uninit instead of unnecessarily writing `size_of::<Node>() *
    // num_nodes` 0s
    unsafe {
        initialize_memory(nodes.as_mut_ptr(), num_nodes, ident);
        nodes.set_len(num_nodes);
    }

    nodes
}

// This takes a raw pointer and a counter because `memory` may be uninitialized.
// `memory` _must_ be a pointer to the beginning of an allocated but
// possibly-uninitialized block of `num_nodes * NODE_BYTES` bytes
//
// We have to use raw pointers to read/write uninit, using "normal" indexing
// causes LLVM to freak out. It counts as a read and causes all writes
// afterwards to be elided. Yes, really. I know, I want to refactor this to use
// less `unsafe` as much as the next rustacean.
unsafe fn initialize_memory(memory: *mut Node, num_nodes: usize, ident: &H256) {
    // We use raw pointers here, see above
    let dst = slice::from_raw_parts_mut(memory as *mut u8, NODE_BYTES);

    debug_assert_eq!(ident.len(), 32);
    keccak_512::write(&ident[..], dst);

    for i in 1..num_nodes {
        // We use raw pointers here, see above
        let dst = slice::from_raw_parts_mut(
            memory.offset(i as _) as *mut u8,
            NODE_BYTES,
        );
        let src = slice::from_raw_parts(
            memory.offset(i as isize - 1) as *mut u8,
            NODE_BYTES,
        );
        keccak_512::write(src, dst);
    }

    // Now this is initialized, we can treat it as a slice.
    let nodes: &mut [Node] = slice::from_raw_parts_mut(memory, num_nodes);

    for _ in 0..POW_CACHE_ROUNDS {
        for i in 0..num_nodes {
            let data_idx = (num_nodes - 1 + i) % num_nodes;
            let idx =
                nodes.get_unchecked_mut(i).as_words()[0] as usize % num_nodes;

            let data = {
                let mut data: Node = nodes.get_unchecked(data_idx).clone();
                let rhs: &Node = nodes.get_unchecked(idx);

                for (a, b) in
                    data.as_dwords_mut().iter_mut().zip(rhs.as_dwords())
                {
                    *a ^= *b;
                }

                data
            };

            keccak_512::write(
                &data.bytes,
                &mut nodes.get_unchecked_mut(i).bytes,
            );
        }
    }
}
