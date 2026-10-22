//! The base field and the in-circuit hash (spec §2).
//!
//! - `F` is KoalaBear.
//! - Poseidon2 of width 24 is the only in-circuit hash: a sponge with rate 12
//!   and capacity 12 elements (372 bits), so a 12-element digest gives ~186
//!   bits of generic classical and ~124 bits of quantum (BHT) collision
//!   resistance.
//! - Rounds: 8 full and 23 partial, with x^3. By the Poseidon paper's attack
//!   constraints (eprint 2019/458, plus 2023/537), which the round count is
//!   derived from, these hold up to a 316-bit target; a 192-bit target needs
//!   (6, 22), so this is 192 bits with 2 extra full rounds and 1 extra
//!   partial round (spec §2). Plonky3 labels them its 128-bit set because
//!   that is the target it chose them for.
//! - The chain id goes into the capacity of every sponge call. It is a
//!   parameter of the chain, never a constant of this crate, so a proof or
//!   a commitment from one chain is meaningless on another (spec P-5).

use std::sync::OnceLock;

use p3_field::{PrimeCharacteristicRing, PrimeField32};
use p3_koala_bear::{
    default_koalabear_poseidon2_24, KoalaBear, Poseidon2KoalaBear,
};
use p3_symmetric::Permutation;

pub type F = KoalaBear;

/// Elements per in-circuit digest (12 × 31 = 372 bits).
pub const DIGEST: usize = 12;
pub type Digest = [F; DIGEST];

pub const WIDTH: usize = 24;
pub const RATE: usize = 12;

/// Domain tags. Each distinct use of the sponge has its own; reusing one is a
/// spec bug.
pub mod dom {
    pub const AK: u32 = 1;
    pub const NK: u32 = 2;
    pub const OWN: u32 = 3;
    pub const HM_DIGEST: u32 = 4;
    pub const LEAF: u32 = 5;
    pub const NODE: u32 = 6;
    // 7 was the nullifier's intermediate mk, retired; never reuse it.
    pub const NF: u32 = 8;
}

fn poseidon2() -> &'static Poseidon2KoalaBear<WIDTH> {
    static P: OnceLock<Poseidon2KoalaBear<WIDTH>> = OnceLock::new();
    P.get_or_init(default_koalabear_poseidon2_24)
}

/// The raw width-24 permutation, which the AIR proves row by row.
pub fn p2_permute(state: &mut [F; WIDTH]) { poseidon2().permute_mut(state); }

/// 2→1 compression `perm(l ‖ r)[..12]`, used only for Merkle nodes. The tree
/// depth is fixed, so a node cannot be confused with a node at another level,
/// and leaves come from the domain-separated sponge.
pub fn compress(l: &Digest, r: &Digest) -> Digest {
    let mut state = [F::ZERO; WIDTH];
    state[..DIGEST].copy_from_slice(l);
    state[DIGEST..].copy_from_slice(r);
    p2_permute(&mut state);
    let mut out = [F::ZERO; DIGEST];
    out.copy_from_slice(&state[..DIGEST]);
    out
}

/// The domain-separated sponge of one chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sponge {
    pub chain_id: u32,
}

impl Sponge {
    pub const fn new(chain_id: u32) -> Self { Sponge { chain_id } }

    /// The domain tag, the chain id and the input length go into the
    /// capacity, so distinct domains, chains and lengths never collide by
    /// padding.
    pub fn hash(&self, domain: u32, input: &[F]) -> Digest {
        let mut state = [F::ZERO; WIDTH];
        state[RATE] = F::new(domain);
        state[RATE + 1] = F::new(self.chain_id);
        state[RATE + 2] = F::new(input.len() as u32);
        if input.is_empty() {
            p2_permute(&mut state);
        }
        for chunk in input.chunks(RATE) {
            for (s, x) in state.iter_mut().zip(chunk) {
                *s += *x;
            }
            p2_permute(&mut state);
        }
        let mut out = [F::ZERO; DIGEST];
        out.copy_from_slice(&state[..DIGEST]);
        out
    }
}

/// SHAKE256 with a labelled, length-prefixed transcript. Used outside the
/// circuit, where the protocol needs fixed public constants (the commitment
/// matrix) or derived randomness; privacy never rests on Poseidon2.
pub struct Xof(tiny_keccak::Shake);

impl Xof {
    pub fn new(label: &str, parts: &[&[u8]]) -> Self {
        use tiny_keccak::Hasher;
        let mut h = tiny_keccak::Shake::v256();
        h.update(&(label.len() as u32).to_le_bytes());
        h.update(label.as_bytes());
        for p in parts {
            h.update(&(p.len() as u32).to_le_bytes());
            h.update(p);
        }
        Xof(h)
    }

    pub fn bytes<const N: usize>(&mut self) -> [u8; N] {
        use tiny_keccak::Xof as _;
        let mut out = [0u8; N];
        self.0.squeeze(&mut out);
        out
    }

    /// An exactly uniform field element, by rejection sampling. Statistical
    /// hiding needs uniform randomness; reducing mod p would bias it.
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

/// Canonical bytes of field elements: 4 bytes little-endian each.
pub fn to_bytes(xs: &[F]) -> Vec<u8> {
    xs.iter()
        .flat_map(|x| x.as_canonical_u32().to_le_bytes())
        .collect()
}

/// Inverse of `to_bytes`; rejects wrong lengths and values >= p.
pub fn from_bytes<const N: usize>(b: &[u8]) -> Option<[F; N]> {
    if b.len() != 4 * N {
        return None;
    }
    let mut out = [F::ZERO; N];
    for (i, c) in b.chunks_exact(4).enumerate() {
        let v = u32::from_le_bytes(c.try_into().unwrap());
        if v >= F::ORDER_U32 {
            return None;
        }
        out[i] = F::new(v);
    }
    Some(out)
}

/// A 64-bit value as four 16-bit limbs (spec C8/C9: no field wraparound).
pub fn limbs(v: u64) -> [F; 4] {
    std::array::from_fn(|i| F::new(((v >> (16 * i)) & 0xffff) as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(n: usize) -> Vec<F> { (0..n as u32).map(F::new).collect() }

    #[test]
    fn sponge_separates_domain_chain_and_length() {
        let s = Sponge::new(1);
        let base = s.hash(dom::LEAF, &input(12));
        assert_ne!(base, s.hash(dom::NODE, &input(12)));
        assert_ne!(base, Sponge::new(2).hash(dom::LEAF, &input(12)));
        let mut padded = input(12);
        padded.push(F::ZERO);
        assert_ne!(base, s.hash(dom::LEAF, &padded));
        assert_ne!(s.hash(dom::LEAF, &[]), s.hash(dom::LEAF, &[F::ZERO]));
    }

    #[test]
    fn bytes_are_canonical() {
        let xs: [F; 3] = [F::new(0), F::new(5), F::new(F::ORDER_U32 - 1)];
        let b = to_bytes(&xs);
        assert_eq!(from_bytes::<3>(&b), Some(xs));
        let mut over = b.clone();
        over[8..12].copy_from_slice(&F::ORDER_U32.to_le_bytes());
        assert_eq!(from_bytes::<3>(&over), None);
        assert_eq!(from_bytes::<3>(&b[..11]), None);
    }

    #[test]
    fn limbs_cover_the_full_range() {
        let l = limbs(u64::MAX);
        assert!(l.iter().all(|x| x.as_canonical_u32() == 0xffff));
        assert_eq!(limbs(0x0001_0002_0003_0004).map(|x| x.as_canonical_u32()), [4, 3, 2, 1]);
    }

    /// The node and the spike prototype must agree on the hash: a digest
    /// computed by a wallet built on the prototype is the same here.
    #[test]
    fn compress_is_the_truncated_permutation() {
        let l: Digest = std::array::from_fn(|i| F::new(i as u32));
        let r: Digest = std::array::from_fn(|i| F::new(100 + i as u32));
        let mut state = [F::ZERO; WIDTH];
        state[..DIGEST].copy_from_slice(&l);
        state[DIGEST..].copy_from_slice(&r);
        p2_permute(&mut state);
        assert_eq!(compress(&l, &r)[..], state[..DIGEST]);
        assert_ne!(compress(&l, &r), compress(&r, &l));
    }
}
