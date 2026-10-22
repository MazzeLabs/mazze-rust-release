//! The 2×2 spend statement (spec §7, C1–C10), ported from the spike's M4
//! suite: honest shapes, targeted attacks pinned to the row that defends
//! against them, random state mutations, and real proofs bound to their
//! Fiat–Shamir domain.

use std::{
    cell::Cell,
    panic::{catch_unwind, AssertUnwindSafe},
};

use mazze_shielded_pq::{
    air::{
        perm::R,
        spend2x2::*,
    },
    field::{Digest, Sponge, Xof, DIGEST, F},
    keys::SpendKeys,
    note::{nullifier, Note},
    ps1::{self, ProverRandomness, PROOF_LEN_MAX},
    tree::{Tree, DEPTH},
    wire,
};
use p3_air::check_constraints;
use p3_field::PrimeCharacteristicRing;
use p3_matrix::dense::RowMajorMatrix;
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

fn accepts(air: &Tx2Air, trace: &RowMajorMatrix<F>, pv: &[F]) -> bool {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let ok = catch_unwind(AssertUnwindSafe(|| check_constraints(air, trace, pv))).is_ok();
    std::panic::set_hook(prev);
    ok
}

/// The first row whose constraints fail, as reported by the debug checker.
fn failing_row(air: &Tx2Air, trace: &RowMajorMatrix<F>, pv: &[F]) -> Option<usize> {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = catch_unwind(AssertUnwindSafe(|| check_constraints(air, trace, pv)));
    std::panic::set_hook(prev);
    let err = r.err()?;
    let msg = err.downcast_ref::<String>().cloned().or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))?;
    let after = msg.split("on row ").nth(1)?;
    after.split(':').next()?.trim().parse().ok()
}

fn honest(w: &Tx2Witness) -> bool {
    let air = Tx2Air::new(CHAIN);
    let (trace, pv) = build_trace(w, CHAIN, HEIGHT, None);
    accepts(&air, &trace, &pv)
}

#[test]
fn honest_shapes_are_accepted_and_match_native() {
    let max = u64::MAX;
    let cases: Vec<(&str, Tx2Witness)> = vec![
        ("payment: 1 real + 1 dummy", tx([Some(100), None], [74, 25], 0, 0, 1)),
        ("two real inputs", tx([Some(60), Some(40)], [99, 0], 0, 0, 1)),
        ("shield-in (both inputs dummy)", tx([None, None], [100, 0], 101, 0, 1)),
        ("shield-out", tx([Some(100), None], [69, 0], 0, 30, 1)),
        ("values near 2^64, carries through every limb", tx([Some(max - 5), Some(5)], [max - 2, 1], 0, 0, 1)),
    ];
    for (name, w) in cases {
        assert!(carries(&w).is_some(), "{name}: fixture must balance");
        assert!(honest(&w), "honest transaction rejected: {name}");
        // Public values agree with the native wallet.
        let (_, pv) = build_trace(&w, CHAIN, HEIGHT, None);
        assert_eq!(&pv[pvi::ANCHOR..pvi::ANCHOR + DIGEST], &w.anchor[..], "{name}");
        assert_eq!(&pv[pvi::CM[0]..pvi::CM[0] + DIGEST], &w.outputs[0].cm(&SPONGE)[..], "{name}");
        let k = SpendKeys::from_nsk(&SPONGE, w.inputs[0].nsk);
        let nf = nullifier(&SPONGE, &k.nk, &w.inputs[0].note.psi, w.inputs[0].pos);
        assert_eq!(&pv[pvi::NF[0]..pvi::NF[0] + DIGEST], &nf[..], "{name}");
    }
}

#[test]
fn attacks_are_rejected() {
    let air = Tx2Air::new(CHAIN);
    let l = layout();

    // 1. Inflation: an output worth 1 more than the inputs. No carry choice can balance it.
    let mut w = tx([Some(100), None], [74, 25], 0, 0, 1);
    w.outputs[0].value += 1;
    assert!(carries(&w).is_none());
    let (_, bal_check) = bal_rows(&l);
    for forged in [[0, 0, 0], [1, 0, 0], [-1, 0, 0], [0, 1, 0], [3, 3, 3]] {
        let (t, pv) = build_trace(&w, CHAIN, HEIGHT, Some(forged));
        let row = failing_row(&air, &t, &pv);
        assert!(row.is_some(), "inflation accepted with carries {forged:?}");
        // Caught by the balance (or, for out-of-range forged carries, by their range block
        // which comes earlier in the trace) — never by anything unrelated.
        let carry_rows: Vec<usize> = range_blocks(&l).iter().filter(|b| matches!(b.2, Target::Carry(_))).map(|b| b.0 + b.1 - 1).collect();
        assert!(row == Some(bal_check) || carry_rows.contains(&row.unwrap()), "inflation caught at unexpected row {row:?}");
    }

    // 2. Wraparound, done consistently: output C carries limb0 = −1 everywhere
    //    (its commitment, its b elements, its register), and output D takes 1
    //    more, so the balance holds limb by limb. Only the range check stands in the way.
    let mut w = tx([Some(100), None], [74, 25], 0, 0, 1);
    w.out_limb_override = [Some([-1, 0, 0, 0]), Some([26 + 74, 0, 0, 0])];
    assert!(carries(&w).is_some(), "the forged transaction balances limb-wise");
    let (t, pv) = build_trace(&w, CHAIN, HEIGHT, None);
    let c_limb0_check = range_blocks(&l).iter().find(|b| b.0 >= l.region[2] * R && matches!(b.2, Target::Val(0))).map(|b| b.0 + b.1 - 1).unwrap();
    let row = failing_row(&air, &t, &pv);
    assert_eq!(row, Some(c_limb0_check), "the field-negative limb must be refused by its range check, got {row:?}");

    // 3. A real-valued input that isn't in the tree (garbage path).
    let mut w = tx([Some(100), None], [74, 25], 0, 0, 1);
    w.inputs[0].path[3] = random_fields();
    let (t, pv) = build_trace(&w, CHAIN, HEIGHT, None);
    let root_row = (l.ins[0].merkle + DEPTH) * R - 1;
    assert_eq!(failing_row(&air, &t, &pv), Some(root_row), "non-member input must fail at the anchor check");

    // 4. ...and trying to switch its membership check off by forging Z = 0.
    let (mut t, pv) = build_trace(&w, CHAIN, HEIGHT, None);
    // A careful forger rewrites every row the registers are held over (including the dummy perm).
    for row in 0..l.region[1] * R {
        t.values[row * COLS + REG_Z] = F::ZERO;
        t.values[row * COLS + REG_INV] = F::ZERO;
    }
    assert_eq!(failing_row(&air, &t, &pv), Some(l.region[0] * R), "forged Z must fail at the gadget on the region's first row");

    // 5. Carries out of range, even when they'd satisfy the limb equations.
    let w = tx([Some(100), None], [74, 25], 0, 0, 1);
    let (t, pv) = build_trace(&w, CHAIN, HEIGHT, Some([1 << 20, 0, 0]));
    assert!(!accepts(&air, &t, &pv), "out-of-range carry accepted");

    // 6. Public amounts: a different fee, and a limb ≥ 2^16.
    let (t, pv) = build_trace(&w, CHAIN, HEIGHT, None);
    let mut p = pv.clone();
    p[pvi::FEE] += F::ONE;
    assert!(!accepts(&air, &t, &p), "wrong public fee accepted");
    let mut p = pv.clone();
    p[pvi::VOUT + 1] = F::new(1 << 16);
    assert!(!accepts(&air, &t, &p), "public limb out of range accepted");

    // 7. Alice spends Bob's note: full opening, her key.
    let w = tx([Some(100), None], [74, 25], 0, 0, 1);
    let mut alice = w.clone();
    alice.inputs[0].nsk = random_fields();
    let (t, pv) = build_trace(&alice, CHAIN, HEIGHT, None);
    assert!(!accepts(&air, &t, &pv), "Alice spent Bob's note");
    assert!(honest(&w), "positive control: Bob's own key passes");

    // 8. Swapping the two nullifiers' order or claiming another nf.
    let (t, pv) = build_trace(&w, CHAIN, HEIGHT, None);
    let mut p = pv.clone();
    for i in 0..DIGEST {
        p.swap(pvi::NF[0] + i, pvi::NF[1] + i);
    }
    assert!(!accepts(&air, &t, &p), "swapped nullifiers accepted");
}

#[test]
fn random_state_mutations_are_rejected() {
    // Every Poseidon2 state cell in a used permutation is constrained (rounds or
    // boundary). Flip random ones; each must be caught.
    let air = Tx2Air::new(CHAIN);
    let w = tx([Some(60), Some(40)], [99, 0], 0, 0, 1);
    let (trace, pv) = build_trace(&w, CHAIN, HEIGHT, None);
    let used_rows = R..layout().sched.len() * R;
    let mut seed = 0x9e3779b97f4a7c15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..120 {
        let row = used_rows.start + (next() as usize) % (used_rows.end - used_rows.start);
        let col = S + (next() as usize) % 24;
        let mut t = trace.clone();
        t.values[row * COLS + col] += F::ONE;
        assert!(!accepts(&air, &t, &pv), "state mutation at row {row}, col {col} NOT detected");
    }
}


fn randomness(seed: u64) -> ProverRandomness {
    ProverRandomness::from_csprng(&mut StdRng::seed_from_u64(seed))
}

/// Real proofs: an honest one verifies through the wire format; the same
/// proof is rejected under another chain, another circuit, other public
/// values, and after any single-bit change.
#[test]
fn proofs_verify_and_are_bound_to_their_domain() {
    let air = Tx2Air::new(CHAIN);
    let w = tx([Some(60), Some(40)], [74, 25], 0, 0, 1);
    let (trace, pv) = build_trace(&w, CHAIN, HEIGHT, None);
    let proof = ps1::prove(&air, CHAIN, trace, &pv, randomness(1)).unwrap();

    let bytes = wire::encode(&proof);
    {
        use bincode::Options;
        let reference = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_little_endian()
            .serialize(&proof)
            .unwrap();
        assert_eq!(bytes, reference, "the wire format is bincode fixint, byte for byte");
    }
    assert!(bytes.len() <= PROOF_LEN_MAX, "proof is {} bytes", bytes.len());
    let decoded = wire::decode(&bytes, PROOF_LEN_MAX).unwrap();
    assert_eq!(wire::encode(&decoded), bytes, "re-encoding is identical");
    ps1::verify(&air, CHAIN, &decoded, &pv).unwrap();

    // Another chain: the AIR constants and the transcript both differ.
    let other_chain = Tx2Air::new(CHAIN + 1);
    assert!(ps1::verify(&other_chain, CHAIN + 1, &decoded, &pv).is_err());
    assert!(ps1::verify(&air, CHAIN + 1, &decoded, &pv).is_err());

    // Other public values: the fee, a nullifier, the sighash.
    for index in [pvi::FEE, pvi::NF[0], pvi::SIGHASH] {
        let mut p = pv.clone();
        p[index] += F::ONE;
        assert!(ps1::verify(&air, CHAIN, &decoded, &p).is_err(), "public value {index} not bound");
    }

    // Single-bit flips are rejected by the decoder or the verifier.
    let mut seed = 0x2545F4914F6CDD1Du64;
    for _ in 0..16 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let pos = (seed as usize) % bytes.len();
        let mut bad = bytes.clone();
        bad[pos] ^= 1 << (seed % 8);
        if let Ok(p) = wire::decode(&bad, PROOF_LEN_MAX) {
            assert!(ps1::verify(&air, CHAIN, &p, &pv).is_err(), "flip at byte {pos} still verifies");
        }
    }

    // The decoder rejects trailing bytes, truncation and anything over the cap.
    let mut longer = bytes.clone();
    longer.push(0);
    assert!(wire::decode(&longer, PROOF_LEN_MAX).is_err());
    assert!(wire::decode(&bytes[..bytes.len() - 1], PROOF_LEN_MAX).is_err());
    assert!(wire::decode(&bytes, bytes.len() - 1).is_err());
}

/// Two proofs of the same statement with different randomness differ (the
/// proof is zero-knowledge only if its randomness is fresh) and both verify.
#[test]
fn prover_randomness_makes_proofs_differ() {
    let air = Tx2Air::new(CHAIN);
    let w = tx([Some(10), None], [9, 0], 0, 0, 1);
    let (trace, pv) = build_trace(&w, CHAIN, HEIGHT, None);
    let a = ps1::prove(&air, CHAIN, trace.clone(), &pv, randomness(2)).unwrap();
    let b = ps1::prove(&air, CHAIN, trace, &pv, randomness(3)).unwrap();
    assert_ne!(wire::encode(&a), wire::encode(&b));
    ps1::verify(&air, CHAIN, &a, &pv).unwrap();
    ps1::verify(&air, CHAIN, &b, &pv).unwrap();
}

/// The constants still give the proven soundness they claim.
#[test]
fn ps1_constants_give_192_proven_bits() {
    use p3_field::coset::TwoAdicMultiplicativeCoset;
    use p3_security::fri::FriRegime;
    use p3_uni_stark::{AirLayout, GrindingSites, OpeningShape, ProvenSecurity, StarkSecurityParams};

    let air = Tx2Air::new(CHAIN);
    let log_n = HEIGHT.trailing_zeros() as usize;
    let params = StarkSecurityParams::from_air::<F, ps1::Challenge, _>(
        FriRegime {
            log_blowup: ps1::LOG_BLOWUP,
            num_queries: ps1::NUM_QUERIES,
            log_final_poly_len: 0,
            max_log_arity: ps1::MAX_LOG_ARITY,
            commit_pow_bits: 0,
            query_pow_bits: ps1::QUERY_POW_BITS,
        },
        &air,
        AirLayout::from_air(&air),
        TwoAdicMultiplicativeCoset::new(F::ONE, log_n).unwrap(),
        248,
        // Collision bits of the 512-bit commitment digest.
        256,
        2,
        OpeningShape::Hiding { num_random_codewords: ps1::NUM_RANDOM_CODEWORDS },
        GrindingSites::NONE,
    );
    let bits = ProvenSecurity::compute_from_proof(log_n, &params).security_bits();
    assert!(bits >= ps1::PROVEN_SECURITY_BITS, "PS-1 gives {bits} proven bits");
}

/// N-UNIQ and the column ties (spec §4.1, §10): every register a nullifier
/// depends on is tied to its source. Changing one cell where it is used must
/// be caught, so no prover can make a second nullifier for a real note.
#[test]
fn nullifier_inputs_are_tied_to_their_sources() {
    let air = Tx2Air::new(CHAIN);
    let l = layout();
    let w = tx([Some(100), None], [74, 25], 0, 0, 1);
    let (trace, pv) = build_trace(&w, CHAIN, HEIGHT, None);
    assert!(accepts(&air, &trace, &pv), "positive control");
    let inl = l.ins[0];
    let mutate = |row: usize, col: usize| {
        let mut t = trace.clone();
        t.values[row * COLS + col] += F::ONE;
        t
    };
    let cases = [
        ("nk register at the mk chunk", inl.mk * R, REG_NK),
        ("psi register at its mk chunk", (inl.mk + 1) * R, REG_PSI),
        ("owner register inside the region", inl.hm * R + 5, REG_OWNER),
        ("position limb (low)", inl.nf * R + R + 1, ACC_LO),
        ("position limb (high)", inl.nf * R + R + 1, ACC_HI),
        ("position chunk", (inl.nf + 1) * R, CHUNK),
        ("a Merkle bit on a non-bit row", inl.merkle * R + 3, BIT),
        ("a value limb register", inl.hm * R + 2, REG_VAL),
        ("the dummy gate", inl.nk * R + 7, REG_Z),
    ];
    for (name, row, col) in cases {
        assert!(!accepts(&air, &mutate(row, col), &pv), "{name} is not tied");
    }
}


