//! M1 tests: the AIR agrees with the native implementation, rejects each class
//! of tampering, and yields a real zero-knowledge proof. Run with
//! `cargo test --release -- --nocapture` to see proof sizes.

use std::panic::{catch_unwind, AssertUnwindSafe};

use p3_air::check_constraints;
use p3_field::PrimeCharacteristicRing;
use p3_matrix::dense::RowMajorMatrix;

use shielded_air_spike::input_air::*;
use shielded_air_spike::stark::*;
use shielded_notes_spike::note::{hm_commit, nullifier, Note};
use shielded_notes_spike::primitives::{random_fields, Digest, DIGEST, F};
use shielded_notes_spike::tree::{root_from_path, DEPTH};

const HEIGHT: usize = 512;

fn fixture(pos: u64) -> (InputWitness, Digest, Digest) {
    let note = Note { d: random_fields(), owner: random_fields(), value: 25, psi: random_fields(), y: random_fields() };
    let (d, b) = hm_commit(&note.message(), &note.y);
    let mut leaf_preimage = [F::ZERO; LEAF_LEN];
    leaf_preimage[..DIGEST].copy_from_slice(&d);
    leaf_preimage[DIGEST..].copy_from_slice(&b);
    let path: [Digest; DEPTH] = std::array::from_fn(|_| random_fields());
    let nk: Digest = random_fields();
    let anchor = root_from_path(&note.cm(), pos, &path);
    let nf = nullifier(&nk, &note.psi, pos);
    (InputWitness { leaf_preimage, path, pos, nk, psi: note.psi }, anchor, nf)
}

fn accepts(air: &InputAir, trace: &RowMajorMatrix<F>, pv: &[F]) -> bool {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let ok = catch_unwind(AssertUnwindSafe(|| check_constraints(air, trace, pv))).is_ok();
    std::panic::set_hook(prev);
    ok
}

#[test]
fn air_matches_native_root_and_nullifier() {
    for pos in [0u64, 5, 65_535, 65_536, (1 << 31) + 7, (1 << 32) - 1] {
        let (w, anchor, nf) = fixture(pos);
        let (trace, pv) = build_trace(&w, HEIGHT);
        assert_eq!(&pv[..DIGEST], &anchor[..], "anchor mismatch at pos {pos}");
        assert_eq!(&pv[DIGEST..], &nf[..], "nullifier mismatch at pos {pos}");
        assert!(accepts(&InputAir::new(HEIGHT), &trace, &pv), "honest trace rejected at pos {pos}");
    }
}

#[test]
fn positions_differing_by_p_have_different_nullifiers() {
    // The bug fixed in this spike: one field element for pos would wrap mod p.
    let p = 2_130_706_433u64;
    let (w, _, _) = fixture(5);
    let nf_a = nullifier(&w.nk, &w.psi, 5);
    let nf_b = nullifier(&w.nk, &w.psi, 5 + p);
    assert_ne!(nf_a, nf_b);
}

#[test]
fn every_tampering_class_is_rejected() {
    let air = InputAir::new(HEIGHT);
    let (w, _, _) = fixture(0b1011_0110_1001);
    let (trace, pv) = build_trace(&w, HEIGHT);
    assert!(accepts(&air, &trace, &pv), "honest trace must pass first");

    let edit = |row: usize, col: usize, delta: u32| {
        let mut t = trace.clone();
        t.values[row * COLS + col] += F::new(delta);
        t
    };
    let merkle_row = 4 + 9;
    let cases: Vec<(&str, RowMajorMatrix<F>, Vec<F>)> = vec![
        ("wrong public anchor", trace.clone(), { let mut p = pv.clone(); p[3] += F::ONE; p }),
        ("wrong public nullifier", trace.clone(), { let mut p = pv.clone(); p[DIGEST + 1] += F::ONE; p }),
        ("altered Merkle sibling", edit(merkle_row, CHUNK + 2, 1), pv.clone()),
        ("flipped direction bit", { let mut t = trace.clone(); let i = merkle_row * COLS + BIT; t.values[i] = F::ONE - t.values[i]; t }, pv.clone()),
        ("non-boolean bit", edit(merkle_row, BIT, 2), pv.clone()),
        ("bit set on a non-Merkle row", edit(1, BIT, 1), pv.clone()),
        ("position accumulator", edit(20, ACC_LO, 1), pv.clone()),
        ("nf position chunk ≠ Merkle position", edit(NF_ROW, CHUNK, 1), pv.clone()),
        ("nonzero sponge padding", edit(3, CHUNK + 7, 1), pv.clone()),
        ("capacity / domain tag", edit(0, 12, 1), pv.clone()),
        ("Poseidon2 internal column", edit(17, 60, 1), pv.clone()),
        ("leaf chunk not chained", edit(2, CHUNK + 1, 1), pv.clone()),
        ("mk chunk changed without nf", edit(37, CHUNK + 4, 1), pv.clone()),
    ];
    for (name, t, p) in cases {
        assert!(!accepts(&air, &t, &p), "tampering NOT detected: {name}");
    }
}

#[test]
fn zk_proof_size_and_soundness() {
    let air = InputAir::new(HEIGHT);
    let (w, _, _) = fixture(123_456);
    for shape in [
        FriShape { cap_height: 0, log_blowup: 4, max_log_arity: 3, query_pow_bits: 16 },
        FriShape { cap_height: 0, log_blowup: 8, max_log_arity: 3, query_pow_bits: 16 },
    ] {
        let (trace, pv) = build_trace(&w, HEIGHT);
        let m = prove_and_verify(&air, trace, &pv, shape, 128).unwrap();
        println!(
            "M1 (1 input: leaf + 32-level Merkle + mk + nf), height {HEIGHT}, blowup 2^{}: \
             {} queries → {} proven bits, proof {:.1} KB [ood {:.1} | query openings {:.1} | FRI layers {:.1} | rest {:.1}], prove {:.0} ms, verify {:.1} ms",
            shape.log_blowup, m.queries, m.bits, m.proof_bytes as f64 / 1024.0,
            m.breakdown[0] as f64 / 1024.0, m.breakdown[1] as f64 / 1024.0, m.breakdown[2] as f64 / 1024.0, m.breakdown[3] as f64 / 1024.0,
            m.prove_ms, m.verify_ms
        );
        assert!(m.bits >= 128);
    }
}

#[test]
fn proof_does_not_verify_against_other_public_values() {
    let air = InputAir::new(HEIGHT);
    let (w, _, _) = fixture(42);
    let (trace, pv) = build_trace(&w, HEIGHT);
    let shape = FriShape { cap_height: 0, log_blowup: 4, max_log_arity: 3, query_pow_bits: 16 };
    assert!(verify_with_other_public(&air, trace.clone(), &pv, &pv, shape), "honest proof must verify");
    let mut forged = pv.clone();
    forged[DIGEST] += F::ONE; // claim a different nullifier
    assert!(!verify_with_other_public(&air, trace, &pv, &forged, shape));
}
