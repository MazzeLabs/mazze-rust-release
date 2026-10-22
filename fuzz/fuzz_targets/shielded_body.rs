//! A shielded body from the network: decoding never panics, and whatever
//! decodes has exactly one encoding and satisfies the structural rules.
#![no_main]
use libfuzzer_sys::fuzz_target;
use primitives::shielded::ShieldedTxBody;

fuzz_target!(|data: &[u8]| {
    if let Ok(body) = ShieldedTxBody::decode_canonical(data) {
        assert_eq!(&rlp::encode(&body)[..], data, "one body, one encoding");
        assert!(body.check_structure().is_ok());
        let _ = (body.hash(), body.sighash(1));
    }
});
