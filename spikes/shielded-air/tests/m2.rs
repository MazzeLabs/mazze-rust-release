//! M2: one input bound end to end (nsk → nk/ak → owner → HM opening → leaf →
//! Merkle → nf) plus one output (HM opening → cm_out), narrow layout.

use std::panic::{catch_unwind, AssertUnwindSafe};

use p3_air::check_constraints;
use p3_field::PrimeCharacteristicRing;
use p3_matrix::dense::RowMajorMatrix;

use shielded_air_spike::stark::*;
use shielded_air_spike::tx_air::*;
use shielded_notes_spike::keys::SpendKeys;
use shielded_notes_spike::note::{nullifier, Note};
use shielded_notes_spike::primitives::{random_fields, Digest, DIGEST, F};
use shielded_notes_spike::tree::{root_from_path, DEPTH};

const HEIGHT: usize = 2048;

struct Fx {
    w: TxWitness,
    anchor: Digest,
    nf: Digest,
    cm_out: Digest,
}

fn fixture(pos: u64) -> Fx {
    let nsk: Digest = random_fields();
    let keys = SpendKeys::from_nsk(nsk);
    let d = random_fields();
    let input = Note { d, owner: keys.owner(&d), value: 25, psi: random_fields(), y: random_fields() };
    let output = Note { d: random_fields(), owner: random_fields(), value: 24, psi: random_fields(), y: random_fields() };
    let path: [Digest; DEPTH] = std::array::from_fn(|_| random_fields());
    Fx {
        anchor: root_from_path(&input.cm(), pos, &path),
        nf: nullifier(&keys.nk, &input.psi, pos),
        cm_out: output.cm(),
        w: TxWitness { nsk, input, path, pos, output },
    }
}

fn accepts(air: &TxAir, trace: &RowMajorMatrix<F>, pv: &[F]) -> bool {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let ok = catch_unwind(AssertUnwindSafe(|| check_constraints(air, trace, pv))).is_ok();
    std::panic::set_hook(prev);
    ok
}

fn pv_of(f: &Fx) -> Vec<F> {
    [f.anchor, f.nf, f.cm_out].concat()
}

#[test]
fn m2_matches_native_commitments_root_nullifier() {
    let air = TxAir::new(HEIGHT);
    for pos in [0u64, 9, (1 << 31) + 3, (1 << 32) - 1] {
        let f = fixture(pos);
        let (trace, pv) = build_trace(&f.w, HEIGHT);
        assert_eq!(pv, pv_of(&f), "public values differ from the native wallet at pos {pos}");
        assert!(accepts(&air, &trace, &pv), "honest trace rejected at pos {pos}");
    }
}

#[test]
fn m2_every_binding_is_enforced() {
    let air = TxAir::new(HEIGHT);
    let f = fixture(0b1100_1010_0101);
    let (trace, pv) = build_trace(&f.w, HEIGHT);
    assert!(accepts(&air, &trace, &pv));
    let r = |perm: usize| perm * R;
    let edit = |row: usize, col: usize| {
        let mut t = trace.clone();
        t.values[row * COLS + col] += F::ONE;
        t
    };
    // Change a register on every row of a region: a "consistent" lie that
    // passes KEEP_REGS and must be caught where the register is pinned or used.
    let edit_region = |rows: std::ops::Range<usize>, col: usize| {
        let mut t = trace.clone();
        for row in rows {
            t.values[row * COLS + col] += F::ONE;
        }
        t
    };
    let input_rows = 0..r(idx::HM_OUT);
    let output_rows = r(idx::HM_OUT)..HEIGHT;
    let cases: Vec<(&str, RowMajorMatrix<F>, Vec<F>)> = vec![
        ("ak from a different nsk than nk", edit(r(idx::AK), CHUNK + 3), pv.clone()),
        ("nsk changed inside the nk permutation", edit(r(idx::NK) + 7, CHUNK + 3), pv.clone()),
        ("NK register, one row", edit(r(idx::MERKLE) + 5, REG_NK + 2), pv.clone()),
        ("NK register, whole input region", edit_region(input_rows.clone(), REG_NK + 2), pv.clone()),
        ("owner absorbs a different nk", edit(r(idx::OWN + 1), CHUNK + 1), pv.clone()),
        ("OWNER register, whole input region", edit_region(input_rows.clone(), REG_OWNER + 4), pv.clone()),
        ("PSI register, whole input region", edit_region(input_rows.clone(), REG_PSI + 1), pv.clone()),
        ("VALUE register, whole input region", edit_region(input_rows.clone(), REG_VAL), pv.clone()),
        ("VALUE register, whole output region", edit_region(output_rows.clone(), REG_VAL + 1), pv.clone()),
        ("OWNER register, whole output region", edit_region(output_rows.clone(), REG_OWNER), pv.clone()),
        ("y element inside an HM permutation", edit(r(idx::HM_IN + 2) + 5, CHUNK + 5), pv.clone()),
        ("A·y accumulator", edit(r(idx::HM_IN + 3) + 20, AY + 9), pv.clone()),
        ("b element of the leaf", edit(r(idx::LEAF_IN + 2), CHUNK + 6), pv.clone()),
        ("mk absorbs a different ψ", edit(r(idx::MK + 1), CHUNK), pv.clone()),
        ("Merkle sibling", edit(r(idx::MERKLE + 4), CHUNK + 2), pv.clone()),
        ("nf position chunk", edit(r(idx::NF + 1), CHUNK), pv.clone()),
        ("public cm_out", trace.clone(), { let mut p = pv.clone(); p[2 * DIGEST + 3] += F::ONE; p }),
        ("public nf", trace.clone(), { let mut p = pv.clone(); p[DIGEST] += F::ONE; p }),
    ];
    for (name, t, p) in cases {
        assert!(!accepts(&air, &t, &p), "binding NOT enforced: {name}");
    }
}

#[test]
fn m2_alice_cannot_spend_bobs_note() {
    // Bob's note is in the tree under `anchor`.
    let f = fixture(17);
    let air = TxAir::new(HEIGHT);
    // Alice knows the whole opening (d, value, ψ, y) but uses her own nsk.
    let mut w = f.w.clone();
    w.nsk = random_fields();
    let (trace, her_pv) = build_trace(&w, HEIGHT);
    // Her trace is internally consistent, but it proves membership of a
    // *different* leaf (her owner tag), so its root is not the real anchor...
    assert_ne!(&her_pv[..DIGEST], &f.anchor[..]);
    // ...and against the real anchor the AIR rejects it.
    let mut real = her_pv.clone();
    real[..DIGEST].copy_from_slice(&f.anchor);
    assert!(!accepts(&air, &trace, &real));
    // Positive control: Bob's own key passes with the real anchor.
    let (bob_trace, bob_pv) = build_trace(&f.w, HEIGHT);
    assert_eq!(&bob_pv[..DIGEST], &f.anchor[..]);
    assert!(accepts(&air, &bob_trace, &bob_pv));
}

#[test]
fn m2_zk_proof() {
    let air = TxAir::new(HEIGHT);
    let f = fixture(4242);
    for log_blowup in [6usize, 8] {
        let (trace, pv) = build_trace(&f.w, HEIGHT);
        let shape = FriShape { cap_height: 0, log_blowup, max_log_arity: 3, query_pow_bits: 16 };
        let m = prove_and_verify(&air, trace, &pv, shape, 128).unwrap();
        println!(
            "M2 (1 full input + 1 output, {COLS} cols × {HEIGHT} rows), blowup 2^{log_blowup}: {} queries → {} proven bits, \
             proof {:.1} KB, prove {:.0} ms, verify {:.1} ms",
            m.queries, m.bits, m.proof_bytes as f64 / 1024.0, m.prove_ms, m.verify_ms
        );
        assert!(m.bits >= 128);
    }
    let (trace, pv) = build_trace(&f.w, HEIGHT);
    let shape = FriShape { cap_height: 0, log_blowup: 6, max_log_arity: 3, query_pow_bits: 16 };
    let mut forged = pv.clone();
    forged[2 * DIGEST] += F::ONE;
    assert!(!verify_with_other_public(&air, trace, &pv, &forged, shape), "forged cm_out must not verify");
}
