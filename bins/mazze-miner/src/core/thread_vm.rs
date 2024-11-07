use mazze_types::{H256, U256};
use randomx_rs::{RandomXCache, RandomXFlag, RandomXVM};

pub struct ThreadLocalVM {
    pub vm: RandomXVM,
    pub cache: RandomXCache,
    pub current_block_hash: H256,
    pub flags: RandomXFlag,
}

impl ThreadLocalVM {
    pub fn new(flags: RandomXFlag, block_hash: &H256) -> Self {
        let cache = RandomXCache::new(flags, block_hash.as_bytes())
            .expect("Failed to create RandomX cache");
        let vm = RandomXVM::new(flags, Some(cache.clone()), None)
            .expect("Failed to create RandomX VM");

        Self {
            vm,
            cache,
            current_block_hash: *block_hash,
            flags,
        }
    }

    pub fn update_if_needed(&mut self, block_hash: &H256) {
        if self.current_block_hash != *block_hash {
            self.cache = RandomXCache::new(self.flags, block_hash.as_bytes())
                .expect("Failed to create RandomX cache");
            self.vm =
                RandomXVM::new(self.flags, Some(self.cache.clone()), None)
                    .expect("Failed to create RandomX VM");
            self.current_block_hash = *block_hash;
        }
    }

    pub fn compute_hash(&self, nonce: &U256, block_hash: &H256) -> H256 {
        let mut input = [0u8; 64];
        input[..32].copy_from_slice(block_hash.as_bytes());
        nonce.to_little_endian(&mut input[32..64]);

        let hash = self
            .vm
            .calculate_hash(&input)
            .expect("Failed to calculate hash");
        H256::from_slice(&hash)
    }
}
