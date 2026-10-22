//! A proof from the network: the decoders never panic nor read past the
//! input, and what decodes re-encodes to the same bytes, padded or not.
#![no_main]
use libfuzzer_sys::fuzz_target;
use mazze_shielded_pq::{ps1::PROOF_LEN_MAX, wire};

fuzz_target!(|data: &[u8]| {
    if let Ok(proof) = wire::decode(data, PROOF_LEN_MAX) {
        assert_eq!(wire::encode(&proof), data, "one proof, one encoding");
    }
    // The padded form: inputs shorter than the cap are padded with zeros, so
    // mutations reach the decoder's padding checks too.
    let mut padded = data.to_vec();
    if padded.len() <= PROOF_LEN_MAX {
        padded.resize(PROOF_LEN_MAX, 0);
        if let Ok(proof) = wire::decode_padded(&padded, PROOF_LEN_MAX) {
            assert_eq!(wire::encode_padded(&proof, PROOF_LEN_MAX).unwrap(), padded);
        }
    }
});
