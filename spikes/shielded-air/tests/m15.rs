//! M1.5: the narrow (one round per row) layout of the M1 statement. Same
//! checks as M1, plus the proof-size comparison.

use std::panic::{catch_unwind, AssertUnwindSafe};

use p3_air::check_constraints;
use p3_field::PrimeCharacteristicRing;
use p3_matrix::dense::RowMajorMatrix;

use shielded_air_spike::input_air::{InputWitness, LEAF_LEN};
use shielded_air_spike::narrow_air::*;
use shielded_air_spike::stark::*;
use shielded_notes_spike::note::{hm_commit, nullifier, Note};
use shielded_notes_spike::primitives::{random_fields, Digest, DIGEST, F};
use shielded_notes_spike::tree::{root_from_path, DEPTH};

const HEIGHT: usize = 2048;
const R: usize = ROWS_PER_PERM;

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

fn accepts(air: &NarrowAir, trace: &RowMajorMatrix<F>, pv: &[F]) -> bool {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let ok = catch_unwind(AssertUnwindSafe(|| check_constraints(air, trace, pv))).is_ok();
    std::panic::set_hook(prev);
    ok
}

#[test]
fn narrow_air_matches_native() {
    let air = NarrowAir::new(HEIGHT);
    for pos in [0u64, 5, 65_535, 65_536, (1 << 31) + 7, (1 << 32) - 1] {
        let (w, anchor, nf) = fixture(pos);
        let (trace, pv) = build_trace(&w, HEIGHT);
        assert_eq!(&pv[..DIGEST], &anchor[..]);
        assert_eq!(&pv[DIGEST..], &nf[..]);
        assert!(accepts(&air, &trace, &pv), "honest trace rejected at pos {pos}");
    }
}

#[test]
fn narrow_every_tampering_class_is_rejected() {
    let air = NarrowAir::new(HEIGHT);
    let (w, _, _) = fixture(0b1011_0110_1001);
    let (trace, pv) = build_trace(&w, HEIGHT);
    assert!(accepts(&air, &trace, &pv));
    let edit = |row: usize, col: usize, delta: u32| {
        let mut t = trace.clone();
        t.values[row * COLS + col] += F::new(delta);
        t
    };
    let merkle_row0 = (5 + 9) * R;
    let cases: Vec<(&str, RowMajorMatrix<F>, Vec<F>)> = vec![
        ("wrong public anchor", trace.clone(), { let mut p = pv.clone(); p[3] += F::ONE; p }),
        ("wrong public nullifier", trace.clone(), { let mut p = pv.clone(); p[DIGEST + 1] += F::ONE; p }),
        ("altered Merkle sibling", edit(merkle_row0, CHUNK + 2, 1), pv.clone()),
        ("flipped direction bit", { let mut t = trace.clone(); let i = merkle_row0 * COLS + BIT; t.values[i] = F::ONE - t.values[i]; t }, pv.clone()),
        ("non-boolean bit", edit(merkle_row0, BIT, 2), pv.clone()),
        ("bit on a non-Merkle row", edit(merkle_row0 + 3, BIT, 1), pv.clone()),
        ("position accumulator", edit(20 * R + 4, ACC_LO, 1), pv.clone()),
        ("nf position chunk ≠ Merkle position", edit(NF_PERM * R, CHUNK, 1), pv.clone()),
        ("nonzero sponge padding", edit(4 * R, CHUNK + 7, 1), pv.clone()),
        ("capacity / domain tag", edit(R, S + 12, 1), pv.clone()),
        ("Poseidon2 round state", edit(17 * R + 10, S + 5, 1), pv.clone()),
        ("partial-round state lane", edit(17 * R + 15, S + 20, 1), pv.clone()),
        ("leaf chunk not chained", edit(2 * R, CHUNK + 1, 1), pv.clone()),
        ("mk chunk changed without nf", edit(38 * R, CHUNK + 4, 1), pv.clone()),
    ];
    for (name, t, p) in cases {
        assert!(!accepts(&air, &t, &p), "tampering NOT detected: {name}");
    }
}

#[test]
fn narrow_zk_proof_size() {
    let air = NarrowAir::new(HEIGHT);
    let (w, _, _) = fixture(123_456);
    for (cap_height, log_blowup) in [(0usize, 8usize)] {
        let shape = FriShape { cap_height, log_blowup, max_log_arity: 3, query_pow_bits: 16 };
        let (trace, pv) = build_trace(&w, HEIGHT);
        let m = prove_and_verify(&air, trace, &pv, shape, 128).unwrap();
        let kb = |b: usize| b as f64 / 1024.0;
        println!(
            "M1.5 narrow ({COLS} cols × {HEIGHT} rows), cap 2^{cap_height}, blowup 2^{log_blowup}: {} queries → {} proven bits, \
             proof {:.1} KB [ood {:.1} | query openings {:.1} | FRI layers {:.1} | rest {:.1}], prove {:.0} ms, verify {:.1} ms",
            m.queries, m.bits, kb(m.proof_bytes), kb(m.breakdown[0]), kb(m.breakdown[1]), kb(m.breakdown[2]), kb(m.breakdown[3]),
            m.prove_ms, m.verify_ms
        );
        assert!(m.bits >= 128);
    }
}

#[test]
fn narrow_proof_rejects_other_public_values() {
    let air = NarrowAir::new(HEIGHT);
    let (w, _, _) = fixture(42);
    let (trace, pv) = build_trace(&w, HEIGHT);
    let shape = FriShape { cap_height: 0, log_blowup: 4, max_log_arity: 3, query_pow_bits: 16 };
    assert!(verify_with_other_public(&air, trace.clone(), &pv, &pv, shape));
    let mut forged = pv.clone();
    forged[DIGEST] += F::ONE;
    assert!(!verify_with_other_public(&air, trace, &pv, &forged, shape));
}

/// Proof size vs trace height (same width): what the full 2×2 statement
/// (≈122 permutations + A·y + ranges → 4096–8192 rows) will cost.
#[test]
fn narrow_size_vs_height() {
    let (w, _, _) = fixture(77);
    for height in [2048usize, 4096, 8192] {
        let air = NarrowAir::new(height);
        let (trace, pv) = build_trace(&w, height);
        let shape = FriShape { cap_height: 0, log_blowup: 8, max_log_arity: 3, query_pow_bits: 16 };
        let m = prove_and_verify(&air, trace, &pv, shape, 128).unwrap();
        println!(
            "height {height}: {} queries, proof {:.1} KB, prove {:.0} ms, verify {:.1} ms",
            m.queries, m.proof_bytes as f64 / 1024.0, m.prove_ms, m.verify_ms
        );
    }
}
