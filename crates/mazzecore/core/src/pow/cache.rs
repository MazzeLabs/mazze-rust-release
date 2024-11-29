use parking_lot::Mutex;
use randomx_rs::{RandomXCache, RandomXFlag, RandomXVM};

use super::{
    compute::Light,
    keccak::{keccak_512, H256},
    seed_compute::SeedHashCompute,
    shared::{
        get_cache_size, Node, NODE_BYTES, POW_CACHE_ROUNDS,
        POW_SEED_HASH_SOURCE_HEIGHT_OFFSET, POW_SEED_HASH_UPDATE_WINDOW,
        POW_STAGE_LENGTH,
    },
};
use std::{mem::zeroed, str::FromStr};

use std::{collections::HashMap, slice, sync::Arc};

pub type Cache = Vec<Node>;

use crossbeam_deque::{Steal, Stealer, Worker};
use parking_lot::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

// Obsolete implementation of the cache builder - octopus specific.
#[derive(Clone)]
pub struct CacheBuilder {
    flags: RandomXFlag,
    seedhash: Arc<Mutex<HashMap<u64, H256>>>,
}

impl CacheBuilder {
    pub fn new() -> Self {
        let flags = RandomXFlag::get_recommended_flags();
        let mut seedhash = HashMap::new();
        seedhash.insert(0, [0u8; 32]);
        CacheBuilder {
            flags,
            seedhash: Arc::new(Mutex::new(seedhash)),
        }
    }

    pub fn get_vm(&self, block_height: u64) -> RandomXVM {
        let cache = self.new_cache(block_height);
        RandomXVM::new(self.flags, Some(cache), None)
            .expect("Failed to create VM")
    }

    pub fn update_seedhash(
        &self, block_hash: &H256, block_height: u64,
    ) -> bool {
        if block_height
            % (POW_STAGE_LENGTH - POW_SEED_HASH_SOURCE_HEIGHT_OFFSET)
            == 0
        {
            let round = (block_height / POW_STAGE_LENGTH) + 1;
            debug!(
                "Updating seedhash for block height: {} - {:?}",
                block_height, block_hash
            );
            self.seedhash.lock().insert(round, block_hash.clone());
            return true;
        }

        false
    }

    fn block_height_to_ident(&self, block_height: u64) -> H256 {
        let block_pos = block_height % POW_STAGE_LENGTH;
        let now_epoch = block_height / POW_STAGE_LENGTH;

        if block_pos < POW_SEED_HASH_UPDATE_WINDOW {
            if now_epoch == 0 {
                return [0u8; 32];
            }

            let prev_epoch = now_epoch - 1;
            return *self.seedhash.lock().get(&prev_epoch).unwrap();
        } else {
            return *self.seedhash.lock().get(&now_epoch).unwrap();
        }
    }

    #[allow(dead_code)]
    fn stage_to_ident(&self, stage: u64) -> H256 {
        *self.seedhash.lock().get(&stage).unwrap()
    }

    pub fn new_cache(&self, block_height: u64) -> RandomXCache {
        let ident = self.block_height_to_ident(block_height);

        RandomXCache::new(self.flags, &ident).expect("Failed to create cache")
    }
}
