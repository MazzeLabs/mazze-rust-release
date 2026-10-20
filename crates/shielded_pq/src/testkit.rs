//! Building real shielded transactions for tests: a tree, real and dummy
//! inputs, a body bound by its sighash, and the proof (wallet side).
//! Randomness is deterministic, so tests are reproducible; never use this
//! for real payments.

use std::cell::Cell;

use p3_field::{PrimeCharacteristicRing, PrimeField32};
use primitives::shielded::{
    proof_hash, RecoverySlot, ShieldedDest, ShieldedDigest, ShieldedTxBody,
};
use rand::{rngs::StdRng, SeedableRng};

use crate::{
    air::spend2x2::*,
    field::{Digest, Sponge, Xof, DIGEST, F},
    keys::SpendKeys,
    note::Note,
    ps1::{self, ProverRandomness},
    tree::{Tree, DEPTH},
    wire,
};

thread_local! {
    static COUNTER: Cell<u64> = const { Cell::new(0) };
}

/// Deterministic, distinct test randomness.
pub fn random_fields<const N: usize>() -> [F; N] {
    let i = COUNTER.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    });
    Xof::new("mazze/test/random", &[&i.to_le_bytes()]).fields()
}

pub fn note_for(chain_id: u32, nsk: &Digest, value: u64) -> Note {
    let d = random_fields();
    let owner = SpendKeys::from_nsk(&Sponge::new(chain_id), *nsk).owner(&Sponge::new(chain_id), &d);
    Note { d, owner, value, psi: random_fields(), y: random_fields() }
}
pub fn out_note(value: u64) -> Note {
    Note { d: random_fields(), owner: random_fields(), value, psi: random_fields(), y: random_fields() }
}
pub fn dummy_in() -> SpendIn {
    let nsk = random_fields();
    SpendIn { note: note_for(0, &nsk, 0), nsk, path: [[F::ZERO; DIGEST]; DEPTH], pos: 0 }
}

/// A tree holding the real inputs (plus some other leaves) and the witness.
pub fn witness(chain_id: u32, in_values: [Option<u64>; 2], outs: [u64; 2], v_in: u64, v_out: u64, fee: u64) -> Tx2Witness {
    let mut tree = Tree::default();
    for _ in 0..5 {
        tree.append(random_fields());
    }
    let mut ins = [dummy_in(), dummy_in()];
    let mut placed = vec![];
    for (i, v) in in_values.iter().enumerate() {
        if let Some(v) = v {
            let nsk = random_fields();
            let note = note_for(chain_id, &nsk, *v);
            let pos = tree.append(note.cm(&Sponge::new(chain_id)));
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
    ShieldedDigest::from_elements(std::array::from_fn(|i| xs[i].as_canonical_u32()))
        .expect("field elements are canonical")
}

/// What a wallet does: fix the body, bind it through the sighash, prove,
/// then commit the body to the proof. Returns the body and the proof bytes.
pub fn prove_transaction(
    chain_id: u32, w: &mut Tx2Witness, dest: Option<ShieldedDest>, seed: u64,
) -> (ShieldedTxBody, Vec<u8>) {
    let (_, pv) = build_trace(w, chain_id, HEIGHT, None);
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
        proof_hash: Default::default(),
    };
    body.check_structure().expect("a well-formed test transaction");
    w.sighash = body.sighash(chain_id);
    let (trace, pv) = build_trace(w, chain_id, HEIGHT, None);
    let air = Tx2Air::new(chain_id);
    let proof = ps1::prove(
        &air,
        chain_id,
        trace,
        &pv,
        ProverRandomness::from_csprng(&mut StdRng::seed_from_u64(seed)),
    )
    .expect("an honest witness proves");
    let bytes = wire::encode(&proof);
    body.proof_hash = proof_hash(&bytes);
    (body, bytes)
}

/// A balanced private payment: one real input of `value`, change and fee.
pub fn payment(chain_id: u32, value: u64, fee: u64, seed: u64) -> (ShieldedTxBody, Vec<u8>) {
    let mut w = witness(chain_id, [Some(value), None], [value - fee, 0], 0, 0, fee);
    prove_transaction(chain_id, &mut w, None, seed)
}
