//! Stage A end to end: a wallet builds a body, proves it, and the node's
//! `verify_body` accepts it; any change to the body or the proof afterwards
//! is rejected.

use std::cell::Cell;

use mazze_shielded_pq::{
    air::spend2x2::*,
    field::{Digest, Sponge, Xof, DIGEST, F},
    keys::SpendKeys,
    note::Note,
    ps1::{self, ProverRandomness, PROOF_LEN_MAX},
    tree::{Tree, DEPTH},
    verify::{verify_body, Rejection},
    wire,
};
use mazze_types::{Address, Space, H512};
use p3_field::{PrimeCharacteristicRing, PrimeField32};
use primitives::shielded::{
    proof_hash, RecoverySlot, ShieldedDest, ShieldedDigest, ShieldedTxBody,
};
use rand::{rngs::StdRng, SeedableRng};

const CHAIN: u32 = 1029;
const SPONGE: Sponge = Sponge::new(CHAIN);

thread_local! {
    static COUNTER: Cell<u64> = const { Cell::new(0) };
}

/// Deterministic, distinct test randomness.
fn random_fields<const N: usize>() -> [F; N] {
    let i = COUNTER.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    });
    Xof::new("mazze/test/random", &[&i.to_le_bytes()]).fields()
}

fn note_for(nsk: &Digest, value: u64) -> Note {
    let d = random_fields();
    let owner = SpendKeys::from_nsk(&SPONGE, *nsk).owner(&SPONGE, &d);
    Note { d, owner, value, psi: random_fields(), y: random_fields() }
}
fn out_note(value: u64) -> Note {
    Note { d: random_fields(), owner: random_fields(), value, psi: random_fields(), y: random_fields() }
}
fn dummy_in() -> SpendIn {
    let nsk = random_fields();
    SpendIn { note: note_for(&nsk, 0), nsk, path: [[F::ZERO; DIGEST]; DEPTH], pos: 0 }
}

/// A tree holding the real inputs (plus some other leaves) and the witness.
fn tx(in_values: [Option<u64>; 2], outs: [u64; 2], v_in: u64, v_out: u64, fee: u64) -> Tx2Witness {
    let mut tree = Tree::default();
    for _ in 0..5 {
        tree.append(random_fields());
    }
    let mut ins = [dummy_in(), dummy_in()];
    let mut placed = vec![];
    for (i, v) in in_values.iter().enumerate() {
        if let Some(v) = v {
            let nsk = random_fields();
            let note = note_for(&nsk, *v);
            let pos = tree.append(note.cm(&SPONGE));
            tree.append(random_fields());
            ins[i] = SpendIn { nsk, note, path: [[F::ZERO; DIGEST]; DEPTH], pos };
            placed.push(i);
        }
    }
    for i in placed {
        ins[i].path = tree.path(ins[i].pos);
    }
    Tx2Witness {
        inputs: ins,
        outputs: [out_note(outs[0]), out_note(outs[1])],
        v_in,
        v_out,
        fee,
        sighash: std::array::from_fn(|i| (i as u8).wrapping_mul(37).wrapping_add(11)),
        anchor: tree.root(),
        out_limb_override: [None, None],
    }
}


fn to_digest(xs: &[F]) -> ShieldedDigest {
    ShieldedDigest::from_elements(std::array::from_fn(|i| xs[i].as_canonical_u32())).unwrap()
}

/// What a wallet does: fix the body, bind it through the sighash, prove,
/// then commit the body to the proof.
fn wallet_tx(w: &mut Tx2Witness, dest: Option<ShieldedDest>) -> (ShieldedTxBody, Vec<u8>) {
    let (_, pv) = build_trace(w, CHAIN, HEIGHT, None);
    let at = |o: usize| to_digest(&pv[o..o + DIGEST]);
    let mut body = ShieldedTxBody {
        pool_version: 1,
        proof_system_id: ps1::PROOF_SYSTEM_ID,
        anchor: at(pvi::ANCHOR),
        nf: [at(pvi::NF[0]), at(pvi::NF[1])],
        cm_out: [at(pvi::CM[0]), at(pvi::CM[1])],
        recovery: RecoverySlot { tag: [1; 32], slot: [2; 64], mac: [3; 16] },
        v_public_in: w.v_in,
        v_public_out: w.v_out,
        dest,
        fee: w.fee,
        proof_hash: H512::zero(),
    };
    body.check_structure().unwrap();
    w.sighash = body.sighash(CHAIN);
    let (trace, pv) = build_trace(w, CHAIN, HEIGHT, None);
    let air = Tx2Air::new(CHAIN);
    let proof = ps1::prove(&air, CHAIN, trace, &pv, ProverRandomness::from_csprng(&mut StdRng::seed_from_u64(7))).unwrap();
    let bytes = wire::encode(&proof);
    body.proof_hash = proof_hash(&bytes);
    (body, bytes)
}

#[test]
fn a_wallet_transaction_passes_stage_a_and_tampering_does_not() {
    let mut w = tx([Some(100), None], [69, 0], 0, 30, 1);
    let dest = Some(ShieldedDest { space: Space::Ethereum, address: Address::repeat_byte(0x42) });
    let (body, proof) = wallet_tx(&mut w, dest);
    verify_body(&body, &proof, CHAIN).expect("honest transaction");

    // The body as a block carries it: encode, decode, verify again.
    let decoded = ShieldedTxBody::decode_canonical(&rlp::encode(&body)).unwrap();
    verify_body(&decoded, &proof, CHAIN).unwrap();

    // Another chain.
    assert!(verify_body(&body, &proof, CHAIN + 1).is_err());

    // Any body change after proving: the sighash (or a public value) moves,
    // and `proof_hash` still matches, so it is the STARK that refuses.
    let tamperings: Vec<(&str, Box<dyn Fn(&mut ShieldedTxBody)>)> = vec![
        ("destination", Box::new(|b| b.dest.as_mut().unwrap().address = Address::repeat_byte(0x66))),
        ("destination space", Box::new(|b| b.dest.as_mut().unwrap().space = Space::Native)),
        ("shield-out amount", Box::new(|b| b.v_public_out += 1)),
        ("fee", Box::new(|b| b.fee += 1)),
        ("recovery slot", Box::new(|b| b.recovery.slot[9] ^= 1)),
        ("nullifier", Box::new(|b| b.nf.swap(0, 1))),
        ("output commitment", Box::new(|b| b.cm_out.swap(0, 1))),
        ("anchor", Box::new(|b| b.anchor = b.cm_out[0])),
    ];
    for (name, tamper) in &tamperings {
        let mut b = body.clone();
        tamper(&mut b);
        assert!(
            matches!(verify_body(&b, &proof, CHAIN), Err(Rejection::InvalidProof(_))),
            "tampered {name} was not refused by the proof"
        );
    }

    // The proof itself: a changed byte no longer matches proof_hash.
    let mut flipped = proof.clone();
    flipped[1000] ^= 1;
    assert_eq!(verify_body(&body, &flipped, CHAIN), Err(Rejection::ProofHashMismatch));
    // A proof re-committed into the body but malformed or wrong.
    let mut recommitted = body.clone();
    recommitted.proof_hash = proof_hash(&flipped);
    assert!(matches!(
        verify_body(&recommitted, &flipped, CHAIN),
        Err(Rejection::InvalidProof(_)) | Err(Rejection::MalformedProof(_))
    ));
    let mut longer = proof.clone();
    longer.push(0);
    recommitted.proof_hash = proof_hash(&longer);
    assert!(matches!(verify_body(&recommitted, &longer, CHAIN), Err(Rejection::MalformedProof(_))));
    // A proof claiming another trace height is refused before the STARK,
    // even when the body is re-committed to it.
    let mut taller = wire::decode(&proof, PROOF_LEN_MAX).unwrap();
    taller.degree_bits += 1;
    let taller = wire::encode(&taller);
    let mut recommitted = body.clone();
    recommitted.proof_hash = proof_hash(&taller);
    assert!(matches!(
        verify_body(&recommitted, &taller, CHAIN),
        Err(Rejection::MalformedProof(m)) if m.contains("trace height")
    ));
    let oversized = vec![0u8; PROOF_LEN_MAX + 1];
    assert_eq!(verify_body(&body, &oversized, CHAIN), Err(Rejection::ProofTooLong(PROOF_LEN_MAX + 1)));

    // Unknown pool version or proof system: refused before any work.
    let mut b = body.clone();
    b.pool_version = 2;
    assert_eq!(verify_body(&b, &proof, CHAIN), Err(Rejection::UnknownPoolVersion(2)));
    let mut b = body.clone();
    b.proof_system_id = 2;
    assert_eq!(verify_body(&b, &proof, CHAIN), Err(Rejection::UnknownProofSystem(2)));
}

/// A proof from a real transaction does not verify for a body that claims a
/// different proof: two honest transactions cannot swap witnesses.
#[test]
fn proofs_do_not_move_between_transactions() {
    let mut w1 = tx([Some(50), None], [49, 0], 0, 0, 1);
    let mut w2 = tx([Some(80), None], [79, 0], 0, 0, 1);
    let (b1, p1) = wallet_tx(&mut w1, None);
    let (b2, p2) = wallet_tx(&mut w2, None);
    verify_body(&b1, &p1, CHAIN).unwrap();
    verify_body(&b2, &p2, CHAIN).unwrap();
    let mut swapped = b1.clone();
    swapped.proof_hash = b2.proof_hash;
    assert!(matches!(verify_body(&swapped, &p2, CHAIN), Err(Rejection::InvalidProof(_))));
}
