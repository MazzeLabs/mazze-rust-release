use crate::core::IntoChunks;
use log::{debug, info, trace};
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

impl From<&AtomicProblemState> for ProblemState {
    fn from(state: &AtomicProblemState) -> Self {
        state.get_problem_details().into()
    }
}

impl From<(u64, H256, U256)> for ProblemState {
    fn from(details: (u64, H256, U256)) -> Self {
        ProblemState::new(details.0, details.1, details.2)
    }
}

#[derive(Debug)]
pub struct AtomicProblemState {
    state: AtomicPtr<ProblemState>,
    solution_submitted: AtomicBool,
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
            solution_submitted: AtomicBool::new(false),
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
            solution_submitted: AtomicBool::new(false),
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

    pub fn matches(&self, block_hash: &H256) -> bool {
        self.get_block_hash() == *block_hash
    }

    pub fn update(&self, new_state: ProblemState) {
        let new_box = Box::into_raw(Box::new(new_state));
        let old_ptr = self.state.swap(new_box, Ordering::Release);

        // SAFETY: old_ptr was created by Box::into_raw and hasn't been freed
        unsafe {
            let _ = Box::from_raw(old_ptr);
        };
        self.solution_submitted.store(false, Ordering::Release);
        trace!("Updated atomic state");
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

    pub fn mark_solution_submitted(&self) {
        self.solution_submitted.store(true, Ordering::Release);
    }

    pub fn has_solution(&self) -> bool {
        self.solution_submitted.load(Ordering::Acquire)
    }

    pub fn get_block_hash(&self) -> H256 {
        self.with_state(|state| H256::from_slice(&state.block_hash))
    }

    pub fn get_boundary(&self) -> U256 {
        self.with_state(|state| U256::from_big_endian(&state.boundary))
    }

    pub fn get_block_height(&self) -> u64 {
        self.with_state(|state| state.block_height)
    }

    pub fn calculate_nonce_range(
        &self, thread_id: usize, num_threads: usize,
    ) -> (U256, U256) {
        self.with_state(|state| {
            let boundary = U256::from_big_endian(&state.boundary);
            let range_size = boundary / U256::from(num_threads);
            let start = range_size * U256::from(thread_id);
            let end = if thread_id == num_threads - 1 {
                U256::from(U256::MAX)
            } else {
                start + range_size
            };
            (start, end)
        })
    }

    #[cfg(target_arch = "x86_64")]
    pub fn check_hash_simd(&self, hash: &H256) -> bool {
        use log::trace;
        unsafe {
            use std::arch::x86_64::{
                __m256i, _mm256_cmpeq_epi8, _mm256_cmpgt_epi8,
                _mm256_loadu_si256, _mm256_movemask_epi8, _mm256_testz_si256,
            };

            self.with_state(|state| {
                trace!("Comparing hash:     {}", hex::encode(hash.as_bytes()));
                trace!("Against boundary:   {}", hex::encode(&state.boundary));

                let boundary_vec = _mm256_loadu_si256(
                    state.boundary.as_ptr() as *const __m256i
                );
                let hash_vec = _mm256_loadu_si256(
                    hash.as_bytes().as_ptr() as *const __m256i
                );

                // First check equality
                let eq = _mm256_cmpeq_epi8(hash_vec, boundary_vec);
                let eq_mask = _mm256_movemask_epi8(eq);

                trace!("Equality mask: {:032b}", eq_mask);

                if eq_mask == -1 {
                    trace!("All bytes equal, returning true");
                    return true; // All bytes equal
                }

                // Find first differing byte
                let first_diff = eq_mask.trailing_ones() as usize;
                trace!("First differing byte at position: {}", first_diff);
                trace!(
                    "Hash byte: {:02x}, Boundary byte: {:02x}",
                    hash.as_bytes()[first_diff],
                    state.boundary[first_diff]
                );

                // Compare the first differing byte
                let result =
                    hash.as_bytes()[first_diff] <= state.boundary[first_diff];
                trace!("Final comparison result: {}", result);

                result
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

    use log::trace;

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
            trace!("Testing valid hash comparison:");
            let simd_result = atomic_state.check_hash_simd(&valid_hash);
            let scalar_result = U256::from(valid_hash.as_bytes()) <= boundary;
            trace!(
                "SIMD result: {}, Scalar result: {}",
                simd_result,
                scalar_result
            );
            assert!(
                simd_result && scalar_result,
                "Valid hash should be accepted by both comparisons"
            );

            trace!("\nTesting invalid hash comparison:");
            let simd_result = atomic_state.check_hash_simd(&invalid_hash);
            let scalar_result = U256::from(invalid_hash.as_bytes()) <= boundary;
            trace!(
                "SIMD result: {}, Scalar result: {}",
                simd_result,
                scalar_result
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

    #[test]
    fn test_real_hash_checks() {
        // Setup the problem state with known values from logs
        let block_hash = H256::from_str(
            "ef6e5a0dd08b7c8be526c5d6ce7d2fcf8e4dd2449d690af4023f4ea989fd2a4e",
        )
        .unwrap();
        let boundary_hex =
            "3fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        let boundary = U256::from_str(boundary_hex).unwrap();
        let atomic_state = AtomicProblemState::new(1, block_hash, boundary);

        // Test a few hashes from the logs
        let test_cases = [
        ("198e31917468e557ef46f80ff185305b0958d68b7eda53bbd0df6da23a725bd1", true),  // 0x19 < 0x3f
        ("1d61d4a5d95daf9ec58ee7072ea48100b0100dc85fc63bfe9f07ee022c8e58fd", true),  // 0x1d < 0x3f
        ("63a5e5f3a46086df9d78224668ad5ce85581bb18147a0d4d135a35d892a01fa7", false), // 0x63 > 0x3f
        ("73c588113916cad6fdd2146a5d72d4ebd45a26fe35ea6cd7cc1d8d014963d6db", false), // 0x73 > 0x3f
        ("ea59e84b76ea17ecf06e03c880e82a56b976bbae8dfe600d1cbe5257ca199adf", false), // 0xea > 0x3f
        ("6bb0008fbcc788b67e80359b8e728b43905f6acdec28b4376193d6f4c6f8408d", false), // 0x6b > 0x3f
        ("ba35cd46b9c7a6079cd61d1461015eff377150837ae20bccd0d6e0ce9781aca6", false), // 0xba > 0x3f
        ("1e7aa1a6b3c139dfdfa065e1aa711888c48fd862492585170c4e2feb797b5a83", true),  // 0x1e < 0x3f
        ("a536802f5e26d5902994d7bb9e4eba3bcf8ce53af5d36cc56c2a85ba454e5bfc", false), // 0xa5 > 0x3f
        ("deb8b119f5fa0e9f7c1ca07ff4fea11e51901b81f026a2c224e9c9072bba6e0d", false), // 0xde > 0x3f
        ("9a8efed3c67219f791e2b2d9c5c861a8f0f604c8a3c908b75a9558a52f08e8b2", false), // 0x9a > 0x3f
        ("21091fddd45ec55e01f14b98485ab4712994b60a9bd3f90600146ffbaed878dc", true),  // 0x21 < 0x3f
        ("c3bb50a4da6ab90933b4b0994074f87b3bbc59489c56a4a99167adb029bcdf42", false), // 0xc3 > 0x3f
        ("08d89890907ae6479457898d0e615971f159e26a6bf036463f08467322b6c881", true),  // 0x08 < 0x3f
        ("8c3c88ac3e888779d03c95d8ba1710ebc70393eaedfb0d1526f98caaed0dad35", false), // 0x8c > 0x3f
        ("e57cc9f23d85d66207ccc5be8da6788c64521531eb143b0a3dc5671a1d524c04", false), // 0xe5 > 0x3f
        ("41c6af5542f170094a3609e7467d3ac87131d4648d24c08755881c754c91146c", false), // 0x41 > 0x3f
        ("149983e823084caf5dd247ba6b0d98cef1aa1a3c1628f55127990a02c90baee2", true),  // 0x14 < 0x3f
        ("a032f762518f1ff371b02db918834ba7f07648e665c3e732fd76839b81d749be", false), // 0xa0 > 0x3f
        ("e04f76a5a408d57b2a2c403bb3f438be108b39484ca621d911a1988d7d48ed66", false), // 0xe0 > 0x3f
        ("5195b8bbf91df660939fcf33a74ca6354844cc7ad9b3792e8b5df4e269c395b2", false), // 0x51 > 0x3f
        ("1a3b61a06efa0d7a6f4db29089bec928b7672c1f87f9d568b8145949ecd20726", true),  // 0x1a < 0x3f
        ("a85850ebbe4ccf742e7df538fb23466cc441f155b53213885e903415888914f9", false), // 0xa8 > 0x3f
        ("274aa77473017dd44480a9752338786bd308d5f81725019ecefd4954b3367f34", true),  // 0x27 < 0x3f
    ];

        for (hash_hex, expected) in test_cases {
            let hash = H256::from_str(hash_hex).unwrap();

            // Test SIMD implementation
            let simd_result = atomic_state.check_hash_simd(&hash);

            // Test scalar comparison for verification
            let scalar_result = U256::from(hash.as_bytes()) <= boundary;

            println!("\nTesting hash: {}", hash_hex);
            println!(
                "SIMD result: {}, Scalar result: {}, Expected: {}",
                simd_result, scalar_result, expected
            );
            println!("First byte: 0x{:02x}", hash.as_bytes()[0]);
            println!("Boundary:   0x{:064x}", boundary);

            assert_eq!(
                simd_result,
                expected,
                "SIMD check failed for hash starting with 0x{:02x}",
                hash.as_bytes()[0]
            );
            assert_eq!(
                scalar_result,
                expected,
                "Scalar check failed for hash starting with 0x{:02x}",
                hash.as_bytes()[0]
            );
        }
    }
}
