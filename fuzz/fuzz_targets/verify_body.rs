//! Stage A on arbitrary input: a body and a proof whose hash is made to
//! match, so every input reaches the STARK verifier. It must never panic (a
//! panic in block acceptance takes the node down). Seeded with real proofs,
//! some inputs verify: those must carry their own body (never the fallback
//! one) and their proof's unique encoding.
#![no_main]
use libfuzzer_sys::fuzz_target;
use mazze_shielded_pq::{ps1::PROOF_LEN_MAX, wire};
use primitives::shielded::{proof_hash, ShieldedTxBody};

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let split = (u16::from_le_bytes([data[0], data[1]]) as usize).min(data.len() - 2);
    let (body_bytes, proof) = data[2..].split_at(split);
    let decoded = ShieldedTxBody::decode_canonical(body_bytes).ok();
    let mut body = decoded
        .clone()
        .unwrap_or_else(|| ShieldedTxBody::sample_for_tests(u32::from(data[0])));
    body.proof_hash = proof_hash(proof);
    if mazze_shielded_pq::verify::verify_body(&body, proof, 1).is_ok() {
        assert!(decoded.is_some(), "a proof verified for a body it does not commit to");
        let decoded = wire::decode_padded(proof, PROOF_LEN_MAX).unwrap();
        let reencoded = wire::encode_padded(&decoded, PROOF_LEN_MAX).unwrap();
        assert_eq!(reencoded, proof, "a verified proof in a non-canonical encoding");
    }
});
