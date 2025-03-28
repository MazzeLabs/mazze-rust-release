use primal::is_prime;

pub const DATASET_BYTES_INIT: u64 = 2 * (1 << 31);
pub const DATASET_BYTES_GROWTH: u64 = 1 << 24;
pub const CACHE_BYTES_INIT: u64 = 2 * (1 << 23);
pub const CACHE_BYTES_GROWTH: u64 = 1 << 16;

pub const POW_STAGE_LENGTH: u64 = 1 << 19;
pub const POW_CACHE_ROUNDS: usize = 3;
pub const POW_MIX_BYTES: usize = 256;
pub const POW_ACCESSES: usize = 32;
pub const POW_DATASET_PARENTS: u32 = 256;
pub const POW_MOD: u32 = 1032193;
pub const POW_MOD_B: u32 = 11;

pub const POW_NK: u64 = 10;
pub const POW_N: u64 = 1 << POW_NK;
pub const POW_WARP_SIZE: u64 = 32;
pub const POW_DATA_PER_THREAD: u64 = POW_N / POW_WARP_SIZE;

pub const NODE_DWORDS: usize = NODE_WORDS / 2;
pub const NODE_WORDS: usize = NODE_BYTES / 4;
pub const NODE_BYTES: usize = 64;

pub fn stage(block_height: u64) -> u64 {
    block_height / POW_STAGE_LENGTH
}

#[allow(dead_code)]
static CHARS: &'static [u8] = b"0123456789abcdef";

#[allow(dead_code)]
pub fn to_hex(bytes: &[u8]) -> String {
    let mut v = Vec::with_capacity(bytes.len() * 2);
    for &byte in bytes.iter() {
        v.push(CHARS[(byte >> 4) as usize]);
        v.push(CHARS[(byte & 0xf) as usize]);
    }

    unsafe { String::from_utf8_unchecked(v) }
}

pub fn get_cache_size(block_height: u64) -> usize {
    // TODO: Memoise
    let mut sz: u64 =
        CACHE_BYTES_INIT + CACHE_BYTES_GROWTH * stage(block_height);
    sz = sz - NODE_BYTES as u64;
    while !is_prime(sz / NODE_BYTES as u64) {
        sz = sz - 2 * NODE_BYTES as u64;
    }
    sz as usize
}