//! Field, hashes and samplers shared by every module.
//!
//! - `F` is KoalaBear, the STARK base field chosen in the P0 prover spike.
//! - `p2_hash` is a Poseidon2 (width 24) sponge: rate 12, capacity 12 elements
//!   (372 bits), so a 12-element digest gives ~186-bit classical / ~124-bit
//!   quantum (BHT) collision resistance. It is used where the circuit needs a
//!   hash: binding, ownership, nullifiers, the Merkle tree.
//! - `Xof` is SHAKE256. It is used for everything outside the circuit that
//!   privacy depends on (randomness derivation, KDFs). Privacy therefore never
//!   rests on Poseidon2.

use std::sync::OnceLock;

use p3_field::{PrimeCharacteristicRing, PrimeField32};
use p3_koala_bear::{default_koalabear_poseidon2_24, KoalaBear, Poseidon2KoalaBear};
use p3_symmetric::Permutation;
use sha3::digest::{ExtendableOutput, Update, XofReader};
use sha3::Shake256;

pub type F = KoalaBear;

/// Elements per in-circuit digest (12 × 31 = 372 bits).
pub const DIGEST: usize = 12;
pub type Digest = [F; DIGEST];

/// Chain identifier mixed into every domain tag (spec P-5).
pub const CHAIN_ID: u32 = 0x4d5a;

pub const WIDTH: usize = 24;
pub const RATE: usize = 12;

fn poseidon2() -> &'static Poseidon2KoalaBear<WIDTH> {
    static P: OnceLock<Poseidon2KoalaBear<WIDTH>> = OnceLock::new();
    P.get_or_init(default_koalabear_poseidon2_24)
}

/// The raw width-24 permutation (shared with the AIR, which proves it row by row).
pub fn p2_permute(state: &mut [F; WIDTH]) {
    poseidon2().permute_mut(state);
}

/// 2→1 compression `perm(l ‖ r)[..12]` (truncated permutation). It is used
/// only for Merkle nodes: one permutation per level. The tree depth is fixed
/// at 32, so a node can't be confused with a node at another level, and leaves
/// come from the domain-separated sponge.
pub fn compress(l: &Digest, r: &Digest) -> Digest {
    let mut state = [F::ZERO; WIDTH];
    state[..DIGEST].copy_from_slice(l);
    state[DIGEST..].copy_from_slice(r);
    p2_permute(&mut state);
    let mut out = [F::ZERO; DIGEST];
    out.copy_from_slice(&state[..DIGEST]);
    out
}

/// Domain-separated Poseidon2 sponge. The domain tag, the chain id and the
/// input length go into the capacity, so distinct domains and lengths can
/// never collide by padding.
pub fn p2_hash(domain: u32, input: &[F]) -> Digest {
    let perm = poseidon2();
    let mut state = [F::ZERO; WIDTH];
    state[RATE] = F::new(domain);
    state[RATE + 1] = F::new(CHAIN_ID);
    state[RATE + 2] = F::new(input.len() as u32);
    if input.is_empty() {
        perm.permute_mut(&mut state);
    }
    for chunk in input.chunks(RATE) {
        for (s, x) in state.iter_mut().zip(chunk) {
            *s += *x;
        }
        perm.permute_mut(&mut state);
    }
    let mut out = [F::ZERO; DIGEST];
    out.copy_from_slice(&state[..DIGEST]);
    out
}

/// SHAKE256 with a labelled, length-prefixed transcript.
pub struct Xof(sha3::Shake256Reader);

impl Xof {
    pub fn new(label: &str, parts: &[&[u8]]) -> Self {
        let mut h = Shake256::default();
        h.update(&(label.len() as u32).to_le_bytes());
        h.update(label.as_bytes());
        for p in parts {
            h.update(&(p.len() as u32).to_le_bytes());
            h.update(p);
        }
        Xof(h.finalize_xof())
    }

    pub fn bytes<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0u8; N];
        self.0.read(&mut out);
        out
    }

    /// Exactly uniform field element by rejection sampling. Statistical hiding
    /// needs the randomness to be uniform; reducing mod p would bias it.
    pub fn field(&mut self) -> F {
        loop {
            let v = u32::from_le_bytes(self.bytes::<4>()) & 0x7fff_ffff;
            if v < F::ORDER_U32 {
                return F::new(v);
            }
        }
    }

    pub fn fields<const N: usize>(&mut self) -> [F; N] {
        std::array::from_fn(|_| self.field())
    }
}

/// Uniform field elements from the OS CSPRNG (fresh, non-derived randomness).
pub fn random_fields<const N: usize>() -> [F; N] {
    let seed: [u8; 32] = rand::random();
    Xof::new("mazze/os-random", &[&seed]).fields()
}

pub fn to_bytes(xs: &[F]) -> Vec<u8> {
    xs.iter().flat_map(|x| x.as_canonical_u32().to_le_bytes()).collect()
}

pub fn from_bytes<const N: usize>(b: &[u8]) -> Option<[F; N]> {
    if b.len() != 4 * N {
        return None;
    }
    let mut out = [F::ZERO; N];
    for (i, c) in b.chunks(4).enumerate() {
        let v = u32::from_le_bytes(c.try_into().unwrap());
        if v >= F::ORDER_U32 {
            return None; // canonical encoding only
        }
        out[i] = F::new(v);
    }
    Some(out)
}

/// 64-bit value as four 16-bit limbs (spec C8/C9: no field wraparound).
pub fn limbs(v: u64) -> [F; 4] {
    std::array::from_fn(|i| F::new(((v >> (16 * i)) & 0xffff) as u32))
}

/// Lower-case RFC 4648 base32 without padding, used for human-facing strings.
pub fn base32(data: &[u8]) -> String {
    const A: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in data {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(A[((buf >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(A[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

pub fn unbase32(s: &str) -> Option<Vec<u8>> {
    let (mut buf, mut bits, mut out) = (0u32, 0u32, Vec::new());
    for c in s.bytes() {
        let v = match c {
            b'a'..=b'z' => c - b'a',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        } as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// Domain tags. Each distinct use of `p2_hash` has its own; reusing one is a
/// spec bug.
pub mod dom {
    pub const AK: u32 = 1;
    pub const NK: u32 = 2;
    pub const OWN: u32 = 3;
    pub const HM_DIGEST: u32 = 4;
    pub const LEAF: u32 = 5;
    pub const NODE: u32 = 6;
    pub const MK: u32 = 7;
    pub const NF: u32 = 8;
}
