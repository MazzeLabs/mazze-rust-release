//! A proof from the network: the decoder never panics nor reads past the
//! cap, and what decodes re-encodes to the same bytes.
#![no_main]
use libfuzzer_sys::fuzz_target;
use mazze_shielded_pq::{ps1::PROOF_LEN_MAX, wire};

fuzz_target!(|data: &[u8]| {
    if let Ok(proof) = wire::decode(data, PROOF_LEN_MAX) {
        assert_eq!(wire::encode(&proof), data, "one proof, one encoding");
    }
});
