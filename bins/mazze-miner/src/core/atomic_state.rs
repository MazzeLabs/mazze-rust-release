use crate::core::IntoChunks;
use mazze_types::{H256, U256};
use mazzecore::pow::ProofOfWorkProblem;
use std::sync::atomic;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

#[derive(Debug)]
pub struct ProblemState {
    block_height: u64,
    block_hash: [u8; 32],
    boundary: [u8; 32],
}

impl ProblemState {
    pub fn new(block_height: u64, block_hash: H256, boundary: U256) -> Self {
        let mut boundary_bytes = [0u8; 32];
        boundary.to_big_endian(&mut boundary_bytes);

        Self {
            block_height,
            block_hash: block_hash.as_bytes().try_into().unwrap(),
            boundary: boundary_bytes,
        }
    }
}

impl From<&ProofOfWorkProblem> for ProblemState {
    fn from(problem: &ProofOfWorkProblem) -> Self {
        let mut boundary_bytes = [0u8; 32];
        problem.boundary.to_big_endian(&mut boundary_bytes);

        Self {
            block_height: problem.block_height,
            block_hash: problem.block_hash.as_bytes().try_into().unwrap(),
            boundary: boundary_bytes,
        }
    }
}

#[derive(Debug)]
pub struct AtomicProblemState {
    state: AtomicPtr<ProblemState>,
    generation: AtomicU64,
}

impl Default for AtomicProblemState {
    fn default() -> Self {
        let initial_state = ProblemState {
            block_height: 0,
            block_hash: H256::zero().as_bytes().try_into().unwrap(),
            boundary: [0u8; 32],
        };
        Self {
            state: AtomicPtr::new(Box::into_raw(Box::new(initial_state))),
            generation: AtomicU64::new(0),
        }
    }
}

impl AtomicProblemState {
    pub fn new(block_height: u64, block_hash: H256, boundary: U256) -> Self {
        let mut boundary_bytes = [0u8; 32];
        boundary.to_big_endian(&mut boundary_bytes);

        let initial_state = ProblemState {
            block_height,
            block_hash: block_hash.as_bytes().try_into().unwrap(),
            boundary: boundary_bytes,
        };
        Self {
            state: AtomicPtr::new(Box::into_raw(Box::new(initial_state))),
            generation: AtomicU64::new(0),
        }
    }

    #[inline]
    fn with_state<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&ProblemState) -> R,
    {
        let ptr = self.state.load(Ordering::Acquire);
        // SAFETY: ptr is always valid due to our update mechanism
        unsafe { f(&*ptr) }
    }

    pub fn update(&self, new_state: ProblemState) {
        let new_box = Box::into_raw(Box::new(new_state));
        let old_ptr = self.state.swap(new_box, Ordering::Release);
        self.generation.fetch_add(1, Ordering::Release);

        // SAFETY: old_ptr was created by Box::into_raw and hasn't been freed
        unsafe { Box::from_raw(old_ptr) };
    }

    pub fn get_problem_details(&self) -> (u64, H256, U256) {
        self.with_state(|state| {
            let boundary = U256::from_big_endian(&state.boundary);

            (
                state.block_height,
                H256::from_slice(&state.block_hash),
                boundary,
            )
        })
    }

    pub fn get_block_hash(&self) -> H256 {
        self.with_state(|state| H256::from_slice(&state.block_hash))
    }

    pub fn get_boundary(&self) -> U256 {
        self.with_state(|state| U256::from_big_endian(&state.boundary))
    }

    pub fn get_generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn calculate_nonce_range(
        &self, thread_id: usize, num_threads: usize,
    ) -> (U256, U256) {
        self.with_state(|state| {
            let boundary = U256::from_big_endian(&state.boundary);
            let range_size = boundary / U256::from(num_threads);
            let start = range_size * U256::from(thread_id);
            let end = if thread_id == num_threads - 1 {
                U256::from(u64::MAX)
            } else {
                start + range_size
            };
            (start, end)
        })
    }

    #[cfg(target_arch = "x86_64")]
    pub fn check_hash_simd(&self, hash: &H256) -> bool {
        unsafe {
            use std::arch::x86_64::{
                __m256i, _mm256_cmpgt_epi8, _mm256_loadu_si256,
                _mm256_testz_si256,
            };

            self.with_state(|state| {
                println!("Boundary: {:?}", hex::encode(&state.boundary));
                println!("Hash:     {:?}", hex::encode(hash.as_bytes()));

                let boundary_vec = _mm256_loadu_si256(
                    state.boundary.as_ptr() as *const __m256i
                );
                let hash_vec = _mm256_loadu_si256(
                    hash.as_bytes().as_ptr() as *const __m256i
                );

                // Compare hash > boundary
                let cmp = _mm256_cmpgt_epi8(hash_vec, boundary_vec);

                // Print the comparison result bits
                let mut cmp_bits = [0u8; 32];
                std::ptr::copy_nonoverlapping(
                    &cmp as *const _ as *const u8,
                    cmp_bits.as_mut_ptr(),
                    32,
                );
                println!("Compare bits: {:?}", hex::encode(&cmp_bits));

                // Test if any bit is set (meaning hash > boundary)
                let has_greater = _mm256_testz_si256(cmp, cmp) == 0;
                println!("Has greater bits: {}", has_greater);

                // Return true if hash <= boundary (no bits set in comparison)
                !has_greater
            })
        }
    }
}

impl PartialEq for AtomicProblemState {
    fn eq(&self, other: &Self) -> bool {
        self.with_state(|self_state| {
            other.with_state(|other_state| {
                // First compare heights since it's just a single u64
                if self_state.block_height != other_state.block_height {
                    return false;
                }

                #[cfg(target_arch = "x86_64")]
                unsafe {
                    use std::arch::x86_64::{
                        __m256i, _mm256_cmpeq_epi64, _mm256_loadu_si256,
                        _mm256_set1_epi64x, _mm256_testc_si256,
                    };

                    // Compare block hashes using SIMD
                    let self_hash = _mm256_loadu_si256(
                        self_state.block_hash.as_ptr() as *const __m256i,
                    );
                    let other_hash = _mm256_loadu_si256(
                        other_state.block_hash.as_ptr() as *const __m256i,
                    );

                    if _mm256_testc_si256(
                        _mm256_cmpeq_epi64(self_hash, other_hash),
                        _mm256_set1_epi64x(-1),
                    ) != 1
                    {
                        return false;
                    }

                    // Compare boundaries using SIMD
                    let self_boundary = _mm256_loadu_si256(
                        self_state.boundary.as_ptr() as *const __m256i,
                    );
                    let other_boundary = _mm256_loadu_si256(
                        other_state.boundary.as_ptr() as *const __m256i,
                    );

                    _mm256_testc_si256(
                        _mm256_cmpeq_epi64(self_boundary, other_boundary),
                        _mm256_set1_epi64x(-1),
                    ) == 1
                }

                #[cfg(not(target_arch = "x86_64"))]
                {
                    self_state.block_hash == other_state.block_hash
                        && self_state.boundary == other_state.boundary
                }
            })
        })
    }
}

impl Drop for AtomicProblemState {
    fn drop(&mut self) {
        // SAFETY: pointer is valid and owned
        unsafe {
            let _ = Box::from_raw(self.state.load(Ordering::Relaxed));
        };
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn test_simd_hash_comparison() {
        #[cfg(target_arch = "x86_64")]
        {
            // Use known values that we can verify
            let boundary_hex = "3fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
            let boundary = U256::from_str(boundary_hex).unwrap();

            // Test hash that we know is valid (less than boundary)
            let valid_hash_hex = "1fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
            let valid_hash =
                H256::from_slice(&hex::decode(valid_hash_hex).unwrap());

            // Test hash that we know is invalid (greater than boundary)
            let invalid_hash_hex = "5fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
            let invalid_hash =
                H256::from_slice(&hex::decode(invalid_hash_hex).unwrap());

            let atomic_state =
                AtomicProblemState::new(1, H256::zero(), boundary);

            // Debug prints for verification
            println!("Testing valid hash comparison:");
            let simd_result = atomic_state.check_hash_simd(&valid_hash);
            let scalar_result = U256::from(valid_hash.as_bytes()) <= boundary;
            println!(
                "SIMD result: {}, Scalar result: {}",
                simd_result, scalar_result
            );
            assert!(
                simd_result && scalar_result,
                "Valid hash should be accepted by both comparisons"
            );

            println!("\nTesting invalid hash comparison:");
            let simd_result = atomic_state.check_hash_simd(&invalid_hash);
            let scalar_result = U256::from(invalid_hash.as_bytes()) <= boundary;
            println!(
                "SIMD result: {}, Scalar result: {}",
                simd_result, scalar_result
            );
            assert!(
                !simd_result && !scalar_result,
                "Invalid hash should be rejected by both comparisons"
            );
        }
    }

    #[test]
    fn test_boundary_conversions() {
        let boundary_hex =
            "3fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        let boundary = U256::from_str(boundary_hex).unwrap();

        let atomic_state = AtomicProblemState::new(1, H256::zero(), boundary);

        let recovered_boundary = atomic_state.get_boundary();
        assert_eq!(boundary, recovered_boundary, "Boundary conversion failed");
    }

    #[test]
    fn test_block_hash_conversions() {
        let block_hash_hex =
            "7dc6e0aad8b74e5ee04e2f34e01b457d017bc4c38c7a5db001e5c7baecbab4e8";
        let block_hash =
            H256::from_slice(&hex::decode(block_hash_hex).unwrap());

        let atomic_state =
            AtomicProblemState::new(1, block_hash, U256::from(1000000));

        let recovered_hash = atomic_state.get_block_hash();
        assert_eq!(block_hash, recovered_hash, "Block hash conversion failed");
    }
}
