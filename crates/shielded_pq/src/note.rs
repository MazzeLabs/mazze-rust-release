//! Notes, the statistically hiding commitment and nullifiers (spec §4).
//!
//! ```text
//! x  = owner(12) ‖ value limbs(4) ‖ ψ(12)        the message, T = 28 elements
//! y  ∈ F^L, L = 64, uniform                      the randomness
//! d  = Poseidon2_HM(y)                           12-element digest
//! b  = x − A·y                                   A ∈ F^{T×L}, public, fixed (SHAKE)
//! cm = Poseidon2_LEAF(d ‖ b)                     the tree leaf
//! ```
//!
//! Hiding is statistical (leftover hash lemma, ≤ 2^-372 when `y` is fresh),
//! with no hash assumption; binding is computational (Poseidon2 collisions).
//! `nf = H_NF(mk, pos)` with `mk = H_MK(nk, ψ)`: `nk` is committed in `owner`,
//! `ψ` in `cm` and `pos` by the Merkle path, so a leaf has exactly one
//! nullifier (N-UNIQ).

use std::sync::OnceLock;

use p3_field::PrimeCharacteristicRing;

use crate::field::{dom, limbs, Digest, Sponge, Xof, DIGEST, F};

pub const HM_T: usize = 12 + 4 + 12;
pub const HM_L: usize = 64;

/// The public matrix `A` (T × L), shared by commitments and the AIR.
pub fn hm_matrix() -> &'static [[F; HM_L]] {
    static A: OnceLock<Vec<[F; HM_L]>> = OnceLock::new();
    A.get_or_init(|| {
        let mut xof = Xof::new("mazze/hm/matrix-A/v1", &[]);
        (0..HM_T).map(|_| xof.fields::<HM_L>()).collect()
    })
}

/// Opened commitment pieces `(d, b)`, before hashing into a leaf.
pub fn hm_commit(
    sponge: &Sponge, x: &[F; HM_T], y: &[F; HM_L],
) -> (Digest, [F; HM_T]) {
    let d = sponge.hash(dom::HM_DIGEST, y);
    let a = hm_matrix();
    let b = std::array::from_fn(|i| {
        let ay = a[i]
            .iter()
            .zip(y)
            .fold(F::ZERO, |acc, (aij, yj)| acc + *aij * *yj);
        x[i] - ay
    });
    (d, b)
}

pub fn leaf(sponge: &Sponge, d: &Digest, b: &[F; HM_T]) -> Digest {
    let mut input = Vec::with_capacity(DIGEST + HM_T);
    input.extend_from_slice(d);
    input.extend_from_slice(b);
    sponge.hash(dom::LEAF, &input)
}

/// The commitment message `owner ‖ value limbs ‖ ψ`.
pub fn message(owner: &Digest, value: u64, psi: &Digest) -> [F; HM_T] {
    let mut x = [F::ZERO; HM_T];
    x[..12].copy_from_slice(owner);
    x[12..16].copy_from_slice(&limbs(value));
    x[16..].copy_from_slice(psi);
    x
}

/// A note. The sender knows every field; the owner's secret `nsk` is not part
/// of it, which is why the sender cannot spend it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    /// Diversifier of the recipient address the note was paid to.
    pub d: [F; 3],
    /// `H(ak, nk, d)` of the recipient (spec §3).
    pub owner: Digest,
    pub value: u64,
    /// Per-note nullifier randomness.
    pub psi: Digest,
    /// Commitment randomness.
    pub y: [F; HM_L],
}

impl Note {
    pub fn message(&self) -> [F; HM_T] {
        message(&self.owner, self.value, &self.psi)
    }

    pub fn cm(&self, sponge: &Sponge) -> Digest {
        let (d, b) = hm_commit(sponge, &self.message(), &self.y);
        leaf(sponge, &d, &b)
    }
}

/// `nf = H_NF(mk, pos)` with `mk = H_MK(nk, ψ)`.
pub fn nullifier(sponge: &Sponge, nk: &Digest, psi: &Digest, pos: u64) -> Digest {
    let mut mk_in = Vec::with_capacity(2 * DIGEST);
    mk_in.extend_from_slice(nk);
    mk_in.extend_from_slice(psi);
    let mk = sponge.hash(dom::MK, &mk_in);
    let mut nf_in = mk.to_vec();
    // Two 16-bit limbs: pos < 2^32 > p, so a single element would wrap mod p
    // and give two positions the same nullifier.
    assert!(pos < 1 << 32, "tree depth is 32");
    nf_in.push(F::new((pos & 0xffff) as u32));
    nf_in.push(F::new(((pos >> 16) & 0xffff) as u32));
    sponge.hash(dom::NF, &nf_in)
}
