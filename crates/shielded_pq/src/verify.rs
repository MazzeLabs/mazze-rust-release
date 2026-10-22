//! Stage A of the verification pipeline (spec §8.2): everything about a
//! shielded transaction that needs no chain state, ending with the STARK.
//! A block is accepted only if this passes for every body in its section.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use p3_field::PrimeField32;
use primitives::shielded::{proof_hash, ShieldedDigest, ShieldedTxBody};

use crate::{
    air::spend2x2::{pvi, Tx2Air, DEGREE_BITS},
    field::{from_bytes, limbs, Digest, DIGEST, F},
    ps1::{self, PROOF_LEN_MAX, PROOF_SYSTEM_ID},
    wire,
};

/// The only pool version this crate proves for.
pub const POOL_VERSION: u8 = 1;

#[derive(Debug, PartialEq, Eq)]
pub enum Rejection {
    UnknownPoolVersion(u8),
    UnknownProofSystem(u8),
    /// Every proof is padded to exactly `PROOF_LEN_MAX` bytes.
    ProofLength(usize),
    ProofHashMismatch,
    MalformedProof(String),
    InvalidProof(String),
}

fn digest(d: &ShieldedDigest) -> Digest {
    from_bytes::<DIGEST>(d.as_bytes())
        .expect("a decoded body holds canonical digests")
}

/// The AIR's public values for `body` (spec §7): anchor, both nullifiers,
/// both output commitments, the public amounts and fee as 16-bit limbs, and
/// the sighash as 16 little-endian 16-bit limbs.
pub fn public_values(body: &ShieldedTxBody, chain_id: u32) -> Vec<F> {
    let mut pv = Vec::with_capacity(pvi::COUNT);
    pv.extend_from_slice(&digest(&body.anchor));
    for nf in &body.nf {
        pv.extend_from_slice(&digest(nf));
    }
    for cm in &body.cm_out {
        pv.extend_from_slice(&digest(cm));
    }
    pv.extend_from_slice(&limbs(body.v_public_in));
    pv.extend_from_slice(&limbs(body.v_public_out));
    pv.extend_from_slice(&limbs(body.fee));
    for pair in body.sighash(chain_id).chunks_exact(2) {
        pv.push(F::new(u16::from_le_bytes([pair[0], pair[1]]) as u32));
    }
    debug_assert_eq!(pv.len(), pvi::COUNT);
    pv
}

/// The statement for one chain, built once: its periodic columns are large.
fn air(chain_id: u32) -> Arc<Tx2Air> {
    static AIRS: OnceLock<Mutex<HashMap<u32, Arc<Tx2Air>>>> = OnceLock::new();
    let mut airs = AIRS.get_or_init(Default::default).lock().unwrap();
    airs.entry(chain_id)
        .or_insert_with(|| Arc::new(Tx2Air::new(chain_id)))
        .clone()
}

/// Verifies one shielded transaction against its proof. Checks run cheapest
/// first; nothing is skipped and nothing is remembered (no "verified" flag).
pub fn verify_body(
    body: &ShieldedTxBody, proof: &[u8], chain_id: u32,
) -> Result<(), Rejection> {
    // The structural rules (distinct nullifiers, a destination exactly
    // when value leaves) again, whatever path built the body: the engine
    // relies on them, and a body with one nullifier twice would spend one
    // note twice.
    body.check_structure().map_err(|e| Rejection::MalformedProof(e.into()))?;
    if body.pool_version != POOL_VERSION {
        return Err(Rejection::UnknownPoolVersion(body.pool_version));
    }
    if body.proof_system_id != PROOF_SYSTEM_ID {
        return Err(Rejection::UnknownProofSystem(body.proof_system_id));
    }
    if proof.len() != PROOF_LEN_MAX {
        return Err(Rejection::ProofLength(proof.len()));
    }
    if proof_hash(proof) != body.proof_hash {
        return Err(Rejection::ProofHashMismatch);
    }
    // The verifier is library code fed with untrusted bytes: a panic in it
    // must reject the proof, never take the node down.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        verify_decoded(body, proof, chain_id)
    }))
    .unwrap_or_else(|_| Err(Rejection::InvalidProof("the verifier panicked".into())))
}

fn verify_decoded(body: &ShieldedTxBody, proof: &[u8], chain_id: u32) -> Result<(), Rejection> {
    let decoded = wire::decode_padded(proof, PROOF_LEN_MAX)
        .map_err(|e| Rejection::MalformedProof(e.to_string()))?;
    if decoded.degree_bits != DEGREE_BITS {
        return Err(Rejection::MalformedProof(format!(
            "trace height 2^{} instead of 2^{}",
            decoded.degree_bits, DEGREE_BITS
        )));
    }
    // Chain ids are field elements in the circuit: two ids that differ by a
    // multiple of p would share every commitment.
    if chain_id >= F::ORDER_U32 {
        return Err(Rejection::MalformedProof("chain id not below p".into()));
    }
    let air = air(chain_id);
    ps1::verify(&*air, chain_id, &decoded, &public_values(body, chain_id))
        .map_err(Rejection::InvalidProof)
}
