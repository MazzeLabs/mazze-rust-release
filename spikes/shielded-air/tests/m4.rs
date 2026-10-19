//! M3 + M4: the complete 2-in × 2-out spend statement. Honest shapes, attacks,
//! a random mutation sweep, and the final ZK proof size.

use std::panic::{catch_unwind, AssertUnwindSafe};

use p3_air::check_constraints;
use p3_field::PrimeCharacteristicRing;
use p3_matrix::dense::RowMajorMatrix;

use shielded_air_spike::stark::*;
use shielded_air_spike::tx2_air::*;
use shielded_air_spike::tx_air::R;
use shielded_notes_spike::keys::SpendKeys;
use shielded_notes_spike::note::{nullifier, Note};
use shielded_notes_spike::primitives::{random_fields, Digest, DIGEST, F};
use shielded_notes_spike::tree::{Tree, DEPTH};

const HEIGHT: usize = 4096;

fn note_for(nsk: &Digest, value: u64) -> Note {
    let d = random_fields();
    Note { d, owner: SpendKeys::from_nsk(*nsk).owner(&d), value, psi: random_fields(), y: random_fields() }
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
            let pos = tree.append(note.cm());
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
    let air = Tx2Air::new(HEIGHT);
    let (trace, pv) = build_trace(w, HEIGHT, None);
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
        let (_, pv) = build_trace(&w, HEIGHT, None);
        assert_eq!(&pv[pvi::ANCHOR..pvi::ANCHOR + DIGEST], &w.anchor[..], "{name}");
        assert_eq!(&pv[pvi::CM[0]..pvi::CM[0] + DIGEST], &w.outputs[0].cm()[..], "{name}");
        let k = SpendKeys::from_nsk(w.inputs[0].nsk);
        let nf = nullifier(&k.nk, &w.inputs[0].note.psi, w.inputs[0].pos);
        assert_eq!(&pv[pvi::NF[0]..pvi::NF[0] + DIGEST], &nf[..], "{name}");
    }
}

#[test]
fn attacks_are_rejected() {
    let air = Tx2Air::new(HEIGHT);
    let l = layout();

    // 1. Inflation: an output worth 1 more than the inputs. No carry choice can balance it.
    let mut w = tx([Some(100), None], [74, 25], 0, 0, 1);
    w.outputs[0].value += 1;
    assert!(carries(&w).is_none());
    let (_, bal_check) = bal_rows(&l);
    for forged in [[0, 0, 0], [1, 0, 0], [-1, 0, 0], [0, 1, 0], [3, 3, 3]] {
        let (t, pv) = build_trace(&w, HEIGHT, Some(forged));
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
    let (t, pv) = build_trace(&w, HEIGHT, None);
    let c_limb0_check = range_blocks(&l).iter().find(|b| b.0 >= l.region[2] * R && matches!(b.2, Target::Val(0))).map(|b| b.0 + b.1 - 1).unwrap();
    let row = failing_row(&air, &t, &pv);
    assert_eq!(row, Some(c_limb0_check), "the field-negative limb must be refused by its range check, got {row:?}");

    // 3. A real-valued input that isn't in the tree (garbage path).
    let mut w = tx([Some(100), None], [74, 25], 0, 0, 1);
    w.inputs[0].path[3] = random_fields();
    let (t, pv) = build_trace(&w, HEIGHT, None);
    let root_row = (l.ins[0].merkle + DEPTH) * R - 1;
    assert_eq!(failing_row(&air, &t, &pv), Some(root_row), "non-member input must fail at the anchor check");

    // 4. ...and trying to switch its membership check off by forging Z = 0.
    let (mut t, pv) = build_trace(&w, HEIGHT, None);
    // A careful forger rewrites every row the registers are held over (including the dummy perm).
    for row in 0..l.region[1] * R {
        t.values[row * COLS + REG_Z] = F::ZERO;
        t.values[row * COLS + REG_INV] = F::ZERO;
    }
    assert_eq!(failing_row(&air, &t, &pv), Some(l.region[0] * R), "forged Z must fail at the gadget on the region's first row");

    // 5. Carries out of range, even when they'd satisfy the limb equations.
    let w = tx([Some(100), None], [74, 25], 0, 0, 1);
    let (t, pv) = build_trace(&w, HEIGHT, Some([1 << 20, 0, 0]));
    assert!(!accepts(&air, &t, &pv), "out-of-range carry accepted");

    // 6. Public amounts: a different fee, and a limb ≥ 2^16.
    let (t, pv) = build_trace(&w, HEIGHT, None);
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
    let (t, pv) = build_trace(&alice, HEIGHT, None);
    assert!(!accepts(&air, &t, &pv), "Alice spent Bob's note");
    assert!(honest(&w), "positive control: Bob's own key passes");

    // 8. Swapping the two nullifiers' order or claiming another nf.
    let (t, pv) = build_trace(&w, HEIGHT, None);
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
    let air = Tx2Air::new(HEIGHT);
    let w = tx([Some(60), Some(40)], [99, 0], 0, 0, 1);
    let (trace, pv) = build_trace(&w, HEIGHT, None);
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

#[test]
fn full_transaction_zk_proof() {
    let air = Tx2Air::new(HEIGHT);
    let w = tx([Some(60), Some(40)], [74, 25], 0, 0, 1);
    for log_blowup in [6usize, 8] {
        let (trace, pv) = build_trace(&w, HEIGHT, None);
        let shape = FriShape { cap_height: 0, log_blowup, max_log_arity: 3, query_pow_bits: 16 };
        let m = prove_and_verify(&air, trace, &pv, shape, 128).unwrap();
        println!(
            "FULL 2×2 TX ({COLS} cols × {HEIGHT} rows, {} perms), blowup 2^{log_blowup}: {} queries → {} proven bits, \
             proof {:.1} KB, prove {:.0} ms, verify {:.1} ms",
            layout().sched.len(), m.queries, m.bits, m.proof_bytes as f64 / 1024.0, m.prove_ms, m.verify_ms
        );
        assert!(m.bits >= 128);
    }
    // The sighash is bound: the same proof under another sighash doesn't verify.
    let (trace, pv) = build_trace(&w, HEIGHT, None);
    let shape = FriShape { cap_height: 0, log_blowup: 6, max_log_arity: 3, query_pow_bits: 16 };
    let mut other = pv.clone();
    other[pvi::SIGHASH] += F::ONE;
    assert!(!verify_with_other_public(&air, trace, &pv, &other, shape), "proof verified under another sighash");
}

#[test]
fn proofs_are_bound_to_chain_proof_system_and_circuit() {
    let air = Tx2Air::new(HEIGHT);
    let w = tx([Some(60), Some(40)], [74, 25], 0, 0, 1);
    let shape = FriShape { cap_height: 0, log_blowup: 6, max_log_arity: 3, query_pow_bits: 16 };
    let ours = Domain::of(&air);
    let (trace, pv) = build_trace(&w, HEIGHT, None);
    assert!(verify_across_domains(&air, trace.clone(), &pv, shape, ours, ours), "same domain must verify");
    for (name, other) in [
        ("another proof system", Domain { proof_system_id: 2, ..ours }),
        ("another chain", Domain { chain_id: ours.chain_id + 1, ..ours }),
        ("another circuit", Domain { air_id: "mazze/shielded/spend-2x2/v2", ..ours }),
    ] {
        assert!(!verify_across_domains(&air, trace.clone(), &pv, shape, ours, other), "proof accepted under {name}");
    }
}

/// Consensus wire format: fixed-width encoding, round trip through verify,
/// length distribution, and rejection of malformed encodings.
#[test]
#[ignore]
fn proof_wire_format() {
    use shielded_air_spike::wire;
    let air = Tx2Air::new(HEIGHT);
    let shape = FriShape { cap_height: 0, log_blowup: 6, max_log_arity: 3, query_pow_bits: 16 };
    let mut lens = vec![];
    let mut keep = None;
    for i in 0..12u64 {
        let w = match i % 3 {
            0 => tx([Some(60 + i), Some(40)], [74 + i, 25], 0, 0, 1),
            1 => tx([Some(1 + i), None], [1 + i, 0], 0, 0, 0),
            _ => tx([Some(u32::MAX as u64), Some(7 + i)], [u32::MAX as u64, 6 + i], 0, 0, 1),
        };
        let (trace, pv) = build_trace(&w, HEIGHT, None);
        let (cfg, proof) = prove_proof(&air, trace, &pv, shape, 128).unwrap();
        let fixed = wire::encode(&proof);
        let post = postcard::to_allocvec(&proof).unwrap().len();
        let decoded = wire::decode(&fixed, 512 * 1024).unwrap();
        verify_proof(&cfg, &air, &decoded, &pv).unwrap();
        assert_eq!(wire::encode(&decoded), fixed, "re-encoding is identical");
        println!("WIRE proof {i}: fixed-width {} B, postcard {} B", fixed.len(), post);
        lens.push(fixed.len());
        keep = Some((cfg, proof, pv, fixed));
    }
    let (min, max) = (*lens.iter().min().unwrap(), *lens.iter().max().unwrap());
    println!("WIRE fixed-width: min {min} B, max {max} B, spread {} B", max - min);

    let (cfg, proof, pv, fixed) = keep.unwrap();
    // Trailing bytes, truncation and the cap are all rejected.
    let mut longer = fixed.clone();
    longer.push(0);
    assert!(wire::decode(&longer, 512 * 1024).is_err());
    assert!(wire::decode(&fixed[..fixed.len() - 1], 512 * 1024).is_err());
    assert!(wire::decode(&fixed, fixed.len() - 1).is_err());
    // A huge length prefix cannot make the decoder allocate past the cap.
    let mut huge = fixed.clone();
    huge[..8].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(wire::decode(&huge, 512 * 1024).is_err());

    // Padded form: constant size, zero padding only.
    let len = max + 4096;
    let padded = wire::encode_padded(&proof, len).unwrap();
    assert_eq!(padded.len(), len);
    let decoded = wire::decode_padded(&padded, len).unwrap();
    verify_proof(&cfg, &air, &decoded, &pv).unwrap();
    let mut dirty = padded.clone();
    *dirty.last_mut().unwrap() = 1;
    assert!(wire::decode_padded(&dirty, len).is_err());
    assert!(wire::decode_padded(&padded[..len - 1], len).is_err());

    // Any single flipped byte in the content is rejected or fails to verify.
    let mut rng = 0x2545F4914F6CDD1Du64;
    for _ in 0..64 {
        rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17;
        let pos = (rng as usize) % fixed.len();
        let mut bad = fixed.clone();
        bad[pos] ^= 1 << (rng % 8);
        if let Ok(p) = wire::decode(&bad, 512 * 1024) {
            assert!(verify_proof(&cfg, &air, &p, &pv).is_err(), "flip at {pos} still verifies");
        }
    }
}
