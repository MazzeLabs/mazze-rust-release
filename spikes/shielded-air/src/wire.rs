//! Consensus wire format for PS-1 proofs (spec §6.2): fixed-width and
//! canonical.
//!
//! Field elements are 4 bytes (Plonky3 writes them in Montgomery form and
//! rejects values >= p), every integer and digest word has a fixed width, and
//! lengths are fixed-width too. A proof therefore has exactly one encoding.
//! The decoder rejects trailing bytes and caps every allocation.

use bincode::Options;
use p3_uni_stark::Proof;

use crate::stark::Config;

pub type Ps1Proof = Proof<Config>;

fn options(limit: u64) -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_little_endian()
        .with_limit(limit)
        .reject_trailing_bytes()
}

pub fn encode(proof: &Ps1Proof) -> Vec<u8> {
    options(u64::MAX).serialize(proof).expect("in-memory proof encodes")
}

/// Decodes at most `max_len` bytes; anything longer is rejected before it is
/// parsed.
pub fn decode(bytes: &[u8], max_len: usize) -> Result<Ps1Proof, String> {
    if bytes.len() > max_len {
        return Err(format!("proof is {} bytes, cap {max_len}", bytes.len()));
    }
    options(max_len as u64).deserialize(bytes).map_err(|e| e.to_string())
}

/// Padded form: the fixed-width encoding followed by zeros up to exactly
/// `len` bytes, so every proof has the same size.
pub fn encode_padded(proof: &Ps1Proof, len: usize) -> Result<Vec<u8>, String> {
    let mut out = encode(proof);
    if out.len() > len {
        return Err(format!("proof is {} bytes, padded size {len}", out.len()));
    }
    out.resize(len, 0);
    Ok(out)
}

pub fn decode_padded(bytes: &[u8], len: usize) -> Result<Ps1Proof, String> {
    if bytes.len() != len {
        return Err(format!("padded proof is {} bytes, expected {len}", bytes.len()));
    }
    let mut reader = bytes;
    let proof: Ps1Proof = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_little_endian()
        .with_limit(len as u64)
        .allow_trailing_bytes()
        .deserialize_from(&mut reader)
        .map_err(|e| e.to_string())?;
    // The rest must be zero padding, and the content must be the canonical
    // encoding of what was parsed.
    if reader.iter().any(|b| *b != 0) {
        return Err("non-zero padding".into());
    }
    if encode(&proof)[..] != bytes[..len - reader.len()] {
        return Err("non-canonical proof content".into());
    }
    Ok(proof)
}
