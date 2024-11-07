use crate::core::BATCH_SIZE;
use mazze_types::{H256, U256};
use randomx_rs::RandomXVM;

pub struct BatchHasher {
    inputs: Vec<Vec<u8>>,
    prefetch_buffer: Vec<u8>,
}

impl BatchHasher {
    pub fn new() -> Self {
        Self {
            inputs: Vec::with_capacity(BATCH_SIZE),
            prefetch_buffer: vec![0u8; 64 * BATCH_SIZE * 2],
        }
    }

    pub fn prepare_batch(&mut self, start_nonce: U256, block_hash: &H256) {
        self.inputs.clear();

        for i in 0..BATCH_SIZE {
            let nonce = start_nonce + i;
            let mut input = vec![0u8; 64];

            // Copy block hash
            input[..32].copy_from_slice(block_hash.as_bytes());

            // Set nonce
            nonce.to_little_endian(&mut input[32..64]);

            self.inputs.push(input);
        }
    }

    pub fn compute_hash_batch(
        &mut self, vm: &RandomXVM, start_nonce: U256, block_hash: &H256,
    ) -> Vec<H256> {
        self.prepare_batch(start_nonce, block_hash);

        let input_refs: Vec<&[u8]> =
            self.inputs.iter().map(|v| v.as_slice()).collect();

        let hashes = vm
            .calculate_hash_set(&input_refs)
            .expect("Failed to calculate hash batch");

        hashes
            .into_iter()
            .map(|hash| H256::from_slice(&hash))
            .collect()
    }

    #[cfg(target_arch = "x86_64")]
    pub unsafe fn compare_hashes_simd(
        &self, hashes: &[H256], boundary: &U256,
    ) -> Option<usize> {
        use std::arch::x86_64::{
            __m256i, _mm256_cmpgt_epi8, _mm256_loadu_si256, _mm256_set1_epi8,
            _mm256_testc_si256,
        };

        let mut boundary_bytes = [0u8; 32];
        boundary.to_little_endian(&mut boundary_bytes);
        let boundary_vec =
            _mm256_loadu_si256(boundary_bytes.as_ptr() as *const __m256i);

        for (i, hash) in hashes.iter().enumerate() {
            let hash_vec =
                _mm256_loadu_si256(hash.as_bytes().as_ptr() as *const __m256i);
            let cmp = _mm256_cmpgt_epi8(boundary_vec, hash_vec);
            if _mm256_testc_si256(cmp, _mm256_set1_epi8(-1)) != 0 {
                return Some(i);
            }
        }
        None
    }
}
