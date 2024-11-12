use log::{debug, warn};
use mazze_types::{H256, U256};
use randomx_rs::RandomXVM;

pub struct RandomXHasher;

impl RandomXHasher {
    pub fn new() -> Self {
        debug!("Creating new RandomXHasher");
        Self
    }

    #[cfg(target_arch = "x86_64")]
    pub fn compute_hash_batch(
        &self, vm: &RandomXVM, start_nonce: U256, block_hash: &H256,
    ) -> Vec<H256> {
        const BATCH_SIZE: usize = 8;
        debug!(
            "Computing hash batch with size {}, starting nonce: {}",
            BATCH_SIZE, start_nonce
        );

        let mut hashes = Vec::with_capacity(BATCH_SIZE);

        for i in 0..BATCH_SIZE {
            let nonce = start_nonce + i;
            let input = Self::create_input(block_hash, nonce);
            match vm.calculate_hash(&input) {
                Ok(hash) => {
                    debug!("Hash calculated for nonce {}", nonce);
                    hashes.push(H256::from_slice(&hash));
                }
                Err(e) => {
                    warn!(
                        "Failed to calculate hash for nonce {}: {}",
                        nonce, e
                    );
                }
            }
        }

        debug!("Completed batch of {} hashes", hashes.len());
        hashes
    }

    fn create_input(block_hash: &H256, nonce: U256) -> Vec<u8> {
        let mut input = Vec::with_capacity(64);
        input.extend_from_slice(block_hash.as_bytes());

        let mut nonce_bytes = [0u8; 32];
        nonce.to_big_endian(&mut nonce_bytes);
        input.extend_from_slice(&nonce_bytes);

        debug!(
            "Created input with block_hash: {}, nonce: {}",
            block_hash, nonce
        );
        input
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use randomx_rs::RandomXFlag;

    #[test]
    fn test_create_input() {
        let block_hash = H256::random();
        let nonce = U256::from(1234u64);
        let input = RandomXHasher::create_input(&block_hash, nonce);
        assert_eq!(input.len(), 64, "Input should be 64 bytes");
    }

    #[test]
    fn test_compute_hash_batch() {
        let hasher = RandomXHasher::new();
        let vm =
            RandomXVM::new(RandomXFlag::get_recommended_flags(), None, None)
                .expect("Failed to create VM");
        let block_hash = H256::random();
        let start_nonce = U256::from(0u64);

        let hashes = hasher.compute_hash_batch(&vm, start_nonce, &block_hash);
        assert_eq!(hashes.len(), 8, "Should compute 8 hashes");
    }
}
