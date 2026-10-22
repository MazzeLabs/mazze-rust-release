//! A block from the network, and a block body from the database: decoding
//! never panics, and a decoded block re-encodes to a fixed point.
#![no_main]
use libfuzzer_sys::fuzz_target;
use primitives::Block;

fuzz_target!(|data: &[u8]| {
    if let Ok(block) = rlp::decode::<Block>(data) {
        let once = rlp::encode(&block);
        let again: Block = rlp::decode(&once).expect("an encoded block decodes");
        assert_eq!(rlp::encode(&again), once);
        assert!(block.witness.is_empty() || block.witness.len() == block.shielded.len());
    }
    let _ = Block::decode_stored_body(data);
});
