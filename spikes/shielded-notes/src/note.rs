//! Notes, the statistically hiding commitment, nullifiers and the note
//! opening that the sender delivers off-chain.
//!
//! Commitment (Halevi–Micali style, over KoalaBear):
//!
//! ```text
//! x  = owner(12) ‖ value limbs(4) ‖ ψ(12)        the message, T = 28 elements
//! y  ∈ F^L, L = 64, uniform                      the randomness
//! d  = Poseidon2_HM(y)                           12-element digest
//! b  = x − A·y                                   A ∈ F^{T×L}, public, fixed (SHAKE)
//! cm = Poseidon2_LEAF(d ‖ b)                     the tree leaf
//! ```
//!
//! **Hiding (statistical).** `y` has `L·log p ≈ 1984` bits of entropy, and `d`
//! reveals at most 372 of them. `y ↦ A·y` is a universal family, so by the
//! leftover hash lemma `A·y` is within `2^-(1984 − 372 − 868)/2 = 2^-372`
//! statistical distance of uniform given `d`. So `b` is independent of `x`,
//! and `cm`, being a function of `(d, b)`, reveals nothing about `x` even to an
//! unbounded adversary. **No hash assumption is involved in hiding.**
//!
//! **Binding (computational).** Two openings of one `cm` give either a
//! Poseidon2 collision in `LEAF`, or the same `(d, b)` with `y ≠ y'`, which is
//! a collision in `HM_DIGEST`. That enables only forward counterfeiting
//! (upgradeable, bounded by the turnstile); it never exposes past notes.
//!
//! **Where the "statistical" claim stops.** It holds when `y` and `ψ` are fresh
//! OS randomness (the no-trace mode). When they are derived from a seed
//! (recipient-blind payments, self-notes), their entropy is the seed's 256
//! bits, and hiding rests on SHAKE256 as a PRF: the symmetric floor, not on
//! any structured problem.

use std::sync::OnceLock;

use p3_field::{PrimeCharacteristicRing, PrimeField32};

use crate::primitives::*;

pub const HM_T: usize = 12 + 4 + 12;
pub const HM_L: usize = 64;

/// Statistical-distance exponent of the hiding argument (bits).
pub fn hiding_bits() -> f64 {
    let lp = (F::ORDER_U32 as f64).log2();
    (HM_L as f64 * lp - DIGEST as f64 * lp - HM_T as f64 * lp) / 2.0
}

fn matrix_a() -> &'static Vec<[F; HM_L]> {
    static A: OnceLock<Vec<[F; HM_L]>> = OnceLock::new();
    A.get_or_init(|| {
        let mut xof = Xof::new("mazze/hm/matrix-A/v1", &[]);
        (0..HM_T).map(|_| xof.fields::<HM_L>()).collect()
    })
}

/// The public matrix `A` (T × L), shared with the AIR so both use the same coefficients.
pub fn hm_matrix() -> &'static Vec<[F; HM_L]> {
    matrix_a()
}

/// Opened commitment pieces, `(d, b)`, before hashing into a leaf.
pub fn hm_commit(x: &[F; HM_T], y: &[F; HM_L]) -> (Digest, [F; HM_T]) {
    let d = p2_hash(dom::HM_DIGEST, y);
    let a = matrix_a();
    let b = std::array::from_fn(|i| {
        let ay = a[i].iter().zip(y).fold(F::ZERO, |acc, (aij, yj)| acc + *aij * *yj);
        x[i] - ay
    });
    (d, b)
}

pub fn leaf(d: &Digest, b: &[F; HM_T]) -> Digest {
    let mut input = Vec::with_capacity(DIGEST + HM_T);
    input.extend_from_slice(d);
    input.extend_from_slice(b);
    p2_hash(dom::LEAF, &input)
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
    /// Per-note nullifier randomness. It makes each nullifier key `mk` fresh,
    /// so spends by the same owner are not jointly solvable.
    pub psi: Digest,
    /// Commitment randomness.
    pub y: [F; HM_L],
}

impl Note {
    pub fn message(&self) -> [F; HM_T] {
        let mut x = [F::ZERO; HM_T];
        x[..12].copy_from_slice(&self.owner);
        x[12..16].copy_from_slice(&limbs(self.value));
        x[16..].copy_from_slice(&self.psi);
        x
    }

    pub fn cm(&self) -> Digest {
        let (d, b) = hm_commit(&self.message(), &self.y);
        leaf(&d, &b)
    }
}

/// `nf = H_NF(mk, pos)` with `mk = H_MK(nk, ψ)`. N-UNIQ: `nk` is committed in
/// `owner`, `ψ` in `cm`, and `pos` by the Merkle path, so one leaf has exactly
/// one nullifier.
pub fn nullifier(nk: &Digest, psi: &Digest, pos: u64) -> Digest {
    let mut mk_in = Vec::with_capacity(2 * DIGEST);
    mk_in.extend_from_slice(nk);
    mk_in.extend_from_slice(psi);
    let mk = p2_hash(dom::MK, &mk_in);
    let mut nf_in = mk.to_vec();
    // Two 16-bit limbs: pos < 2^32 > p, so a single element would wrap mod p
    // and give two positions the same nullifier.
    assert!(pos < 1 << 32, "tree depth is 32");
    nf_in.push(F::new((pos & 0xffff) as u32));
    nf_in.push(F::new(((pos >> 16) & 0xffff) as u32));
    p2_hash(dom::NF, &nf_in)
}

/// What the sender hands to the recipient off-chain. The owner is not in it:
/// the recipient recomputes it from its own keys and `d`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteOpening {
    pub d: [F; 3],
    pub value: u64,
    pub psi: Digest,
    pub y: [F; HM_L],
}

pub const OPENING_VERSION: u8 = 1;
pub const OPENING_LEN: usize = 1 + 12 + 8 + 4 * DIGEST + 4 * HM_L;

impl NoteOpening {
    pub fn of(note: &Note) -> Self {
        NoteOpening { d: note.d, value: note.value, psi: note.psi, y: note.y }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = vec![OPENING_VERSION];
        v.extend(to_bytes(&self.d));
        v.extend(self.value.to_le_bytes());
        v.extend(to_bytes(&self.psi));
        v.extend(to_bytes(&self.y));
        debug_assert_eq!(v.len(), OPENING_LEN);
        v
    }

    /// Strict canonical decoding: exact length, known version, canonical
    /// field elements.
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() != OPENING_LEN || b[0] != OPENING_VERSION {
            return None;
        }
        let d = from_bytes::<3>(&b[1..13])?;
        let value = u64::from_le_bytes(b[13..21].try_into().ok()?);
        let psi = from_bytes::<DIGEST>(&b[21..21 + 48])?;
        let y = from_bytes::<HM_L>(&b[69..])?;
        Some(NoteOpening { d, value, psi, y })
    }

    /// The "plain" form: for an in-person QR code, NFC, a file or paper.
    pub fn to_plain(&self) -> String {
        format!("mzn1{}", base32(&self.to_bytes()))
    }

    pub fn from_plain(s: &str) -> Option<Self> {
        Self::from_bytes(&unbase32(s.strip_prefix("mzn1")?)?)
    }

    pub fn into_note(self, owner: Digest) -> Note {
        Note { d: self.d, owner, value: self.value, psi: self.psi, y: self.y }
    }
}
