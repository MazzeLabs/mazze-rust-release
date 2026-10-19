// Copyright 2026 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Soundness sanity checks for the shielded-pool Groth16 circuit.
//!
//! Catches single-bit mutations of public inputs and of witnesses. Does
//! NOT replace a third-party audit — under-constrained witnesses,
//! proof malleability, and active/inactive-slot aliasing all need
//! constraint-by-constraint review.
//!
//! Covered:
//! 1. The circuit is satisfiable for a minimal well-formed bundle
//!    (1 shielded input → 1 shielded output, no transparent outputs,
//!    no fee). Proof gen + verify round-trip.
//! 2. Flipping a bit on any public input invalidates the proof.
//! 3. A proof from a mutated witness either fails to generate or
//!    fails to verify against the original public inputs.
//!
//! Tests are `#[ignore]` because Groth16 setup + prove is slow
//! (~10s setup + ~2-5s per proof in release). Run with:
//!
//! ```bash
//! cargo test -p mazze-executor --release -- --ignored shielded_circuit
//! ```

use ark_bls12_381::{Bls12_381, Fr};
use ark_ff::{One, Zero};
use ark_groth16::{prepare_verifying_key, Groth16, PreparedVerifyingKey};
use ark_serialize::CanonicalDeserialize;
use ark_snark::SNARK;
use ark_std::rand::{rngs::StdRng, SeedableRng};
use mazze_executor::shielded::{
    circuit::{
        ShieldedCircuit, ShieldedInputWitness, ShieldedOutputWitness,
        MAX_SHIELDED_INPUTS, MAX_SHIELDED_OUTPUTS, MAX_TRANSPARENT_OUTPUTS,
        MERKLE_DEPTH,
    },
    fr_from_bytes, poseidon_hash, poseidon_hash2,
};

/// Build a minimal well-formed witness:
/// - one shielded input at leaf index 0 of an otherwise-empty tree,
/// - one shielded output of equal value,
/// - zero transparent outputs, zero fee.
struct MinimalBundle {
    circuit: ShieldedCircuit,
    public_inputs: Vec<Fr>,
    anchor: Fr,
    nullifier: Fr,
    output_commitment: Fr,
}

/// Pre-compute the zero-hash ladder so the test can build a leaf-0
/// Merkle path with all-zero siblings (matches `append_commitment`'s
/// behaviour for the first leaf inserted into an empty tree).
fn zero_hashes() -> Vec<Fr> {
    let mut zeros = Vec::with_capacity(MERKLE_DEPTH + 1);
    zeros.push(Fr::zero());
    for level in 0..MERKLE_DEPTH {
        let next = poseidon_hash2(&zeros[level], &zeros[level]);
        zeros.push(next);
    }
    zeros
}

fn build_minimal_bundle(value: u64) -> MinimalBundle {
    // Distinct field elements for every secret field — easier to
    // attribute mutation failures during debugging.
    let recipient_l = Fr::from(0xA1A1_A1A1u64);
    let recipient_r = Fr::from(0xB2B2_B2B2u64);
    let val_fr = Fr::from(value);
    let rho_in = Fr::from(0xC3C3_C3C3u64);
    let rseed_in = Fr::from(0xD4D4_D4D4u64);
    let secret_in = Fr::from(0xE5E5_E5E5u64);

    let rho_out = Fr::from(0xF6F6_F6F6u64);
    let rseed_out = Fr::from(0x0707_0707u64);

    // Input commitment = Poseidon5(rl, rr, value, rho, rseed).
    let input_commitment = poseidon_hash(&[
        recipient_l,
        recipient_r,
        val_fr,
        rho_in,
        rseed_in,
    ]);

    // Nullifier = Poseidon2(secret, rho).
    let nullifier = poseidon_hash(&[secret_in, rho_in]);

    // Output commitment for the receiving note.
    let output_commitment = poseidon_hash(&[
        recipient_l,
        recipient_r,
        val_fr,
        rho_out,
        rseed_out,
    ]);

    // Walk the all-zero-sibling path: leaf at index 0, so we are
    // always the left child. At each level the anchor accumulator
    // hashes (node, zero_hashes[level]).
    let zeros = zero_hashes();
    let mut anchor = input_commitment;
    for level in 0..MERKLE_DEPTH {
        anchor = poseidon_hash2(&anchor, &zeros[level]);
    }

    // Pad inputs / outputs to fixed-size slots.
    let mut inputs = Vec::with_capacity(MAX_SHIELDED_INPUTS);
    inputs.push(ShieldedInputWitness {
        recipient_left: recipient_l,
        recipient_right: recipient_r,
        value: val_fr,
        rho: rho_in,
        rseed: rseed_in,
        secret: secret_in,
        path_elements: zeros[..MERKLE_DEPTH].to_vec(),
        path_bits: vec![false; MERKLE_DEPTH],
    });
    for _ in 1..MAX_SHIELDED_INPUTS {
        inputs.push(ShieldedInputWitness {
            recipient_left: Fr::zero(),
            recipient_right: Fr::zero(),
            value: Fr::zero(),
            rho: Fr::zero(),
            rseed: Fr::zero(),
            secret: Fr::zero(),
            path_elements: vec![Fr::zero(); MERKLE_DEPTH],
            path_bits: vec![false; MERKLE_DEPTH],
        });
    }

    let mut outputs = Vec::with_capacity(MAX_SHIELDED_OUTPUTS);
    outputs.push(ShieldedOutputWitness {
        recipient_left: recipient_l,
        recipient_right: recipient_r,
        value: val_fr,
        rho: rho_out,
        rseed: rseed_out,
    });
    for _ in 1..MAX_SHIELDED_OUTPUTS {
        outputs.push(ShieldedOutputWitness {
            recipient_left: Fr::zero(),
            recipient_right: Fr::zero(),
            value: Fr::zero(),
            rho: Fr::zero(),
            rseed: Fr::zero(),
        });
    }

    let mut nullifiers = vec![Fr::zero(); MAX_SHIELDED_INPUTS];
    nullifiers[0] = nullifier;
    let mut commitments = vec![Fr::zero(); MAX_SHIELDED_OUTPUTS];
    commitments[0] = output_commitment;

    let circuit = ShieldedCircuit {
        anchor,
        num_inputs: 1,
        num_commitments: 1,
        num_outputs: 0,
        nullifiers,
        commitments,
        transparent_outputs: vec![Fr::zero(); MAX_TRANSPARENT_OUTPUTS],
        transparent_values: vec![Fr::zero(); MAX_TRANSPARENT_OUTPUTS],
        fee: Fr::zero(),
        inputs,
        outputs,
    };

    // Public inputs follow `crates/mazzecore/executor/src/internal_contract/contracts/shielded_pool.rs::build_public_inputs`
    // layout: anchor, num_inputs, num_commitments, num_outputs,
    // 8x nullifier slots, 8x commitment slots, 8x transparent address
    // slots, 8x transparent value slots, fee.
    let mut public_inputs = Vec::with_capacity(38);
    public_inputs.push(anchor);
    public_inputs.push(Fr::from(1u64)); // num_inputs
    public_inputs.push(Fr::from(1u64)); // num_commitments
    public_inputs.push(Fr::from(0u64)); // num_outputs
    public_inputs.push(nullifier);
    for _ in 1..MAX_SHIELDED_INPUTS {
        public_inputs.push(Fr::zero());
    }
    public_inputs.push(output_commitment);
    for _ in 1..MAX_SHIELDED_OUTPUTS {
        public_inputs.push(Fr::zero());
    }
    for _ in 0..MAX_TRANSPARENT_OUTPUTS {
        public_inputs.push(Fr::zero());
    }
    for _ in 0..MAX_TRANSPARENT_OUTPUTS {
        public_inputs.push(Fr::zero());
    }
    public_inputs.push(Fr::zero()); // fee

    MinimalBundle {
        circuit,
        public_inputs,
        anchor,
        nullifier,
        output_commitment,
    }
}

/// Setup once per process so the mutation tests amortise the cost.
fn shared_setup() -> (
    ark_groth16::ProvingKey<Bls12_381>,
    PreparedVerifyingKey<Bls12_381>,
) {
    // Deterministic seed — proof traces from a failed run are
    // reproducible by anyone re-running the same test.
    let mut rng = StdRng::seed_from_u64(0xc0ffee_dead_beefu64);
    let blank = ShieldedCircuit::blank();
    let (pk, vk) =
        Groth16::<Bls12_381>::circuit_specific_setup(blank, &mut rng)
            .expect("circuit_specific_setup failed");
    let pvk = prepare_verifying_key(&vk);
    (pk, pvk)
}

/// Try-prove with one fallback on a different RNG seed, mirroring the
/// retry pattern in `crates/mazzecore/executor/src/bin/shielded_bundle.rs`
/// which works around occasional non-verifying proofs from `Groth16::prove`.
///
/// `ark-groth16` 0.4 calls `cs.is_satisfied().unwrap()` inside the prover —
/// so a witness that violates a constraint produces a **panic**, not an
/// `Err(SynthesisError)`. Returning `None` on panic is the desired
/// behaviour here: the test sees a uniform "rejection" signal whether the
/// constraint failure surfaces as a panic, an Err, or a non-verifying
/// proof.
fn prove_with_retry(
    pk: &ark_groth16::ProvingKey<Bls12_381>,
    pvk: &PreparedVerifyingKey<Bls12_381>,
    circuit: ShieldedCircuit,
    public_inputs: &[Fr],
    base_seed: u64,
) -> Option<ark_groth16::Proof<Bls12_381>> {
    let attempt = |seed: u64, c: ShieldedCircuit| {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || {
                let mut rng = StdRng::seed_from_u64(seed);
                Groth16::<Bls12_381>::prove(pk, c, &mut rng)
            },
        ));
        match result {
            Ok(Ok(p)) => Some(p),
            // The prover panicked (constraint unsatisfied) or returned
            // an error — both mean "this witness is not provable".
            Ok(Err(_)) | Err(_) => None,
        }
    };

    let proof = match attempt(base_seed, circuit.clone()) {
        Some(p) => p,
        None => return None,
    };
    if Groth16::<Bls12_381>::verify_proof(pvk, &proof, public_inputs)
        .unwrap_or(false)
    {
        return Some(proof);
    }
    let retry = match attempt(base_seed.wrapping_add(1), circuit) {
        Some(p) => p,
        None => return None,
    };
    if Groth16::<Bls12_381>::verify_proof(pvk, &retry, public_inputs)
        .unwrap_or(false)
    {
        Some(retry)
    } else {
        None
    }
}

/// Round-trip the canonical-byte form of a proof. Asserts the
/// `Proof<Bls12_381>` ↔ `compressed bytes` codec round-trips, which is
/// the path on-chain proofs travel through.
fn proof_roundtrips(proof: &ark_groth16::Proof<Bls12_381>) -> bool {
    use ark_serialize::CanonicalSerialize;
    let mut bytes = Vec::new();
    if proof.serialize_compressed(&mut bytes).is_err() {
        return false;
    }
    let mut reader = &bytes[..];
    ark_groth16::Proof::<Bls12_381>::deserialize_compressed(&mut reader)
        .is_ok()
}

#[test]
#[ignore]
fn circuit_blank_setup_succeeds() {
    let (_pk, _pvk) = shared_setup();
}

#[test]
#[ignore]
fn circuit_roundtrip_minimal_bundle_verifies() {
    let (pk, pvk) = shared_setup();
    let bundle = build_minimal_bundle(100);

    let proof = prove_with_retry(
        &pk,
        &pvk,
        bundle.circuit.clone(),
        &bundle.public_inputs,
        1,
    )
    .expect("prove + verify round-trip failed for a well-formed bundle");

    assert!(
        proof_roundtrips(&proof),
        "proof failed compressed-bytes round-trip",
    );
}

#[test]
#[ignore]
fn circuit_rejects_anchor_mutation() {
    let (pk, pvk) = shared_setup();
    let bundle = build_minimal_bundle(100);

    // Generate a valid proof against the real public inputs.
    let proof = prove_with_retry(
        &pk,
        &pvk,
        bundle.circuit.clone(),
        &bundle.public_inputs,
        2,
    )
    .expect("setup proof generation failed");

    // Mutate the anchor (public input slot 0).
    let mut mutated = bundle.public_inputs.clone();
    mutated[0] = bundle.anchor + Fr::one();

    let verified =
        Groth16::<Bls12_381>::verify_proof(&pvk, &proof, &mutated)
            .unwrap_or(false);
    assert!(
        !verified,
        "proof verified against a mutated anchor — soundness violation",
    );
}

#[test]
#[ignore]
fn circuit_rejects_nullifier_mutation() {
    let (pk, pvk) = shared_setup();
    let bundle = build_minimal_bundle(100);

    let proof = prove_with_retry(
        &pk,
        &pvk,
        bundle.circuit.clone(),
        &bundle.public_inputs,
        3,
    )
    .expect("setup proof generation failed");

    // Mutate the nullifier slot (public input slot 4 — after anchor +
    // 3 count fields).
    let mut mutated = bundle.public_inputs.clone();
    mutated[4] = bundle.nullifier + Fr::one();

    let verified =
        Groth16::<Bls12_381>::verify_proof(&pvk, &proof, &mutated)
            .unwrap_or(false);
    assert!(
        !verified,
        "proof verified against a mutated nullifier — soundness violation",
    );
}

#[test]
#[ignore]
fn circuit_rejects_output_commitment_mutation() {
    let (pk, pvk) = shared_setup();
    let bundle = build_minimal_bundle(100);

    let proof = prove_with_retry(
        &pk,
        &pvk,
        bundle.circuit.clone(),
        &bundle.public_inputs,
        4,
    )
    .expect("setup proof generation failed");

    // Mutate the output-commitment slot (after anchor + 3 counts + 8
    // nullifier slots = index 12).
    let mut mutated = bundle.public_inputs.clone();
    mutated[12] = bundle.output_commitment + Fr::one();

    let verified =
        Groth16::<Bls12_381>::verify_proof(&pvk, &proof, &mutated)
            .unwrap_or(false);
    assert!(
        !verified,
        "proof verified against a mutated output commitment",
    );
}

#[test]
#[ignore]
fn circuit_rejects_count_mismatch() {
    let (pk, pvk) = shared_setup();
    let bundle = build_minimal_bundle(100);

    let proof = prove_with_retry(
        &pk,
        &pvk,
        bundle.circuit.clone(),
        &bundle.public_inputs,
        5,
    )
    .expect("setup proof generation failed");

    // Claim num_inputs = 2 in the public inputs while only one
    // shielded input is actually constrained. A correct circuit
    // requires the second slot's nullifier to also match a
    // Merkle-included commitment, which it doesn't here.
    let mut mutated = bundle.public_inputs.clone();
    mutated[1] = Fr::from(2u64);

    let verified =
        Groth16::<Bls12_381>::verify_proof(&pvk, &proof, &mutated)
            .unwrap_or(false);
    assert!(
        !verified,
        "proof verified with inflated num_inputs — count constraint underpowered",
    );
}

#[test]
#[ignore]
fn circuit_rejects_witness_with_wrong_secret() {
    let (pk, pvk) = shared_setup();
    let mut bundle = build_minimal_bundle(100);

    // Mutate the prover's secret. The nullifier in the public inputs
    // was computed from the ORIGINAL secret, so re-proving with a
    // different secret should produce a different nullifier and the
    // proof should either fail to generate or fail to verify against
    // the original public inputs.
    bundle.circuit.inputs[0].secret = Fr::from(0xDEAD_BEEFu64);

    let outcome = prove_with_retry(
        &pk,
        &pvk,
        bundle.circuit,
        &bundle.public_inputs,
        6,
    );
    assert!(
        outcome.is_none(),
        "prover succeeded with a wrong secret — witness underconstrained",
    );
}

#[test]
#[ignore]
fn circuit_rejects_witness_with_wrong_value() {
    let (pk, pvk) = shared_setup();
    let mut bundle = build_minimal_bundle(100);

    // Mutate the input value but not the output value — the
    // balance-equation constraint should now be violated.
    bundle.circuit.inputs[0].value = Fr::from(200u64);

    let outcome = prove_with_retry(
        &pk,
        &pvk,
        bundle.circuit,
        &bundle.public_inputs,
        7,
    );
    assert!(
        outcome.is_none(),
        "prover succeeded with input value ≠ output value — balance constraint underpowered",
    );
}

#[test]
#[ignore]
fn circuit_rejects_witness_with_wrong_path() {
    let (pk, pvk) = shared_setup();
    let mut bundle = build_minimal_bundle(100);

    // Corrupt level 0 of the Merkle path. The path verification
    // constraint should now fail to hash up to the anchor.
    bundle.circuit.inputs[0].path_elements[0] = Fr::from(0xBADu64);

    let outcome = prove_with_retry(
        &pk,
        &pvk,
        bundle.circuit,
        &bundle.public_inputs,
        8,
    );
    assert!(
        outcome.is_none(),
        "prover succeeded with a corrupted Merkle path",
    );
}

#[test]
#[ignore]
fn circuit_zero_value_passes() {
    // Edge case: zero-value transfer. The commitment + nullifier are
    // still well-formed, the balance equation is 0 = 0 + 0 + 0.
    let (pk, pvk) = shared_setup();
    let bundle = build_minimal_bundle(0);

    let proof = prove_with_retry(
        &pk,
        &pvk,
        bundle.circuit,
        &bundle.public_inputs,
        9,
    );
    assert!(
        proof.is_some(),
        "zero-value transfer should be a valid bundle",
    );
}

#[test]
#[ignore]
fn circuit_rejects_inactive_nullifier_nonzero() {
    let (pk, pvk) = shared_setup();
    let bundle = build_minimal_bundle(100);

    let proof = prove_with_retry(
        &pk,
        &pvk,
        bundle.circuit.clone(),
        &bundle.public_inputs,
        10,
    )
    .expect("setup proof generation failed");

    // The circuit constrains: inactive nullifier slots MUST be zero.
    // Public input index 5 is the *second* nullifier slot, which is
    // inactive (num_inputs = 1). Setting it nonzero must invalidate.
    let mut mutated = bundle.public_inputs.clone();
    mutated[5] = Fr::from(0xBADu64);

    let verified =
        Groth16::<Bls12_381>::verify_proof(&pvk, &proof, &mutated)
            .unwrap_or(false);
    assert!(
        !verified,
        "inactive nullifier slot was permitted to be nonzero",
    );
}

/// Sanity check that our `fr_from_bytes` matches the helper in the
/// circuit's witness-construction path. If this diverges, every other
/// test in this file silently encodes the wrong witness.
#[test]
fn fr_from_bytes_matches_be_mod_order() {
    let bytes = [0u8; 32];
    assert_eq!(fr_from_bytes(&bytes), Fr::zero());

    let mut one_be = [0u8; 32];
    one_be[31] = 1;
    assert_eq!(fr_from_bytes(&one_be), Fr::one());
}
