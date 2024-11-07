use crate::core::IntoChunks;
use mazze_types::{H256, U256};
use mazzecore::pow::ProofOfWorkProblem;
use std::sync::atomic;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Clone)]
pub struct AtomicStateSnapshot {
    pub block_height: u64,
    pub block_hash: [u64; 4],
}

#[derive(Debug)]
pub struct AtomicProblemState {
    pub block_height: AtomicU64,
    pub block_hash: [AtomicU64; 4],
    pub boundary: [AtomicU64; 4],
    pub is_active: AtomicBool,
}

impl Default for AtomicProblemState {
    fn default() -> Self {
        Self {
            block_height: AtomicU64::new(0),
            block_hash: H256::zero().into_chunks().map(AtomicU64::new),
            boundary: U256::zero().into_chunks().map(AtomicU64::new),
            is_active: AtomicBool::new(false),
        }
    }
}

impl AtomicProblemState {
    pub fn new(
        block_height_arg: u64, block_hash_arg: H256, boundary_arg: U256,
    ) -> Self {
        Self {
            block_height: AtomicU64::new(block_height_arg),
            block_hash: block_hash_arg.into_chunks().map(AtomicU64::new),
            boundary: boundary_arg.into_chunks().map(AtomicU64::new),
            is_active: AtomicBool::new(false),
        }
    }

    pub fn new_from(problem: &AtomicProblemState) -> Self {
        let mut state = Self::default();
        state.update(problem);
        state
    }

    pub fn snapshot(&self) -> AtomicStateSnapshot {
        AtomicStateSnapshot {
            block_height: self.block_height.load(Ordering::Acquire),
            block_hash: [
                self.block_hash[0].load(Ordering::Acquire),
                self.block_hash[1].load(Ordering::Acquire),
                self.block_hash[2].load(Ordering::Acquire),
                self.block_hash[3].load(Ordering::Acquire),
            ],
        }
    }

    pub fn update(&self, problem: &AtomicProblemState) {
        self.block_height.store(
            problem.block_height.load(Ordering::Acquire),
            Ordering::Release,
        );

        for i in 0..4 {
            self.block_hash[i].store(
                problem.block_hash[i].load(Ordering::Acquire),
                Ordering::Release,
            );
            self.boundary[i].store(
                problem.boundary[i].load(Ordering::Acquire),
                Ordering::Release,
            );
        }
    }

    pub fn from_problem(problem: &AtomicProblemState) -> Self {
        let state = Self::default();
        state.update(problem);
        state
    }

    pub fn get_block_hash(&self) -> H256 {
        // Use Relaxed ordering since we have a fence in get_problem_details
        let mut bytes = [0u8; 32];
        for i in 0..4 {
            let chunk = self.block_hash[i].load(Ordering::Relaxed);
            bytes[i * 8..(i + 1) * 8].copy_from_slice(&chunk.to_le_bytes());
        }
        H256::from_slice(&bytes)
    }

    pub fn get_boundary(&self) -> U256 {
        // Use Relaxed ordering since we have a fence in get_problem_details
        let mut bytes = [0u8; 32];
        for i in 0..4 {
            let chunk = self.boundary[i].load(Ordering::Relaxed);
            bytes[i * 8..(i + 1) * 8].copy_from_slice(&chunk.to_le_bytes());
        }
        U256::from_little_endian(&bytes)
    }

    pub fn get_problem_details(&self) -> (u64, H256, U256) {
        atomic::fence(Ordering::Acquire);

        let height = self.block_height.load(Ordering::Relaxed);
        let hash = self.get_block_hash();
        let boundary = self.get_boundary();

        (height, hash, boundary)
    }

    pub fn calculate_nonce_range(
        &self, thread_id: usize, num_threads: usize,
    ) -> (U256, U256) {
        // Focus on lower nonce ranges first
        let range_size =
            U256::from(self.get_boundary()) / U256::from(num_threads);
        let start = range_size * U256::from(thread_id);
        let end = if thread_id == num_threads - 1 {
            U256::from(u64::MAX)
        } else {
            start + range_size
        };
        (start, end)
    }
}

impl PartialEq for AtomicProblemState {
    fn eq(&self, other: &Self) -> bool {
        // First compare heights since it's just a single u64
        if self.block_height.load(Ordering::Relaxed)
            != other.block_height.load(Ordering::Relaxed)
        {
            return false;
        }

        #[cfg(target_arch = "x86_64")]
        unsafe {
            use std::arch::x86_64::{
                __m256i, _mm256_cmpeq_epi64, _mm256_loadu_si256,
                _mm256_set1_epi64x, _mm256_testc_si256,
            };

            // Compare block hashes using SIMD
            let self_hash =
                _mm256_loadu_si256(self.block_hash.as_ptr() as *const __m256i);
            let other_hash =
                _mm256_loadu_si256(other.block_hash.as_ptr() as *const __m256i);
            let hash_cmp = _mm256_cmpeq_epi64(self_hash, other_hash);
            if _mm256_testc_si256(hash_cmp, _mm256_set1_epi64x(-1)) != 1 {
                return false;
            }

            // Compare boundaries using SIMD
            let self_boundary =
                _mm256_loadu_si256(self.boundary.as_ptr() as *const __m256i);
            let other_boundary =
                _mm256_loadu_si256(other.boundary.as_ptr() as *const __m256i);
            let boundary_cmp =
                _mm256_cmpeq_epi64(self_boundary, other_boundary);
            _mm256_testc_si256(boundary_cmp, _mm256_set1_epi64x(-1)) == 1
        }

        #[cfg(not(target_arch = "x86_64"))]
        {
            // Fallback for non-x86_64 architectures
            self.get_block_hash() == other.get_block_hash()
                && self.get_boundary() == other.get_boundary()
        }
    }
}

// Implement Eq since all our components can be exactly equal
impl Eq for AtomicProblemState {}

// Implement PartialOrd
impl PartialOrd for AtomicProblemState {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// Implement Ord - we'll primarily order by block height
impl Ord for AtomicProblemState {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // First compare block heights
        let self_height = self.block_height.load(Ordering::Relaxed);
        let other_height = other.block_height.load(Ordering::Relaxed);

        self_height
            .cmp(&other_height)
            // If heights are equal, compare block hashes
            .then_with(|| self.get_block_hash().cmp(&other.get_block_hash()))
            // If hashes are equal, compare boundaries
            .then_with(|| self.get_boundary().cmp(&other.get_boundary()))
    }
}

// Also implement PartialEq with ProofOfWorkProblem for convenience
impl PartialEq<ProofOfWorkProblem> for AtomicProblemState {
    fn eq(&self, other: &ProofOfWorkProblem) -> bool {
        self.block_height.load(Ordering::Relaxed) == other.block_height
            && self.get_block_hash() == other.block_hash
            && self.get_boundary() == other.boundary
    }
}

// And the reverse comparison
impl PartialEq<AtomicProblemState> for ProofOfWorkProblem {
    fn eq(&self, other: &AtomicProblemState) -> bool {
        other == self
    }
}

// Implement PartialOrd with ProofOfWorkProblem
impl PartialOrd<ProofOfWorkProblem> for AtomicProblemState {
    fn partial_cmp(
        &self, other: &ProofOfWorkProblem,
    ) -> Option<std::cmp::Ordering> {
        Some(
            self.block_height
                .load(Ordering::Relaxed)
                .cmp(&other.block_height),
        )
    }
}

// And the reverse comparison
impl PartialOrd<AtomicProblemState> for ProofOfWorkProblem {
    fn partial_cmp(
        &self, other: &AtomicProblemState,
    ) -> Option<std::cmp::Ordering> {
        other.partial_cmp(self).map(|ord| ord.reverse())
    }
}

impl Into<AtomicProblemState> for ProofOfWorkProblem {
    fn into(self) -> AtomicProblemState {
        AtomicProblemState::new(
            self.block_height,
            self.block_hash,
            self.boundary,
        )
    }
}

impl IntoChunks for H256 {
    fn into_chunks(self) -> [u64; 4] {
        let bytes = self.as_bytes();
        [
            u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
            u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
        ]
    }
}

impl IntoChunks for U256 {
    fn into_chunks(self) -> [u64; 4] {
        let mut bytes = [0u8; 32];
        self.to_little_endian(&mut bytes);
        [
            u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
            u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
        ]
    }
}
