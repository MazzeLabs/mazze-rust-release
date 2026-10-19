//! M1: the per-input part of the spend statement as a real AIR.
//!
//! Statement: "I know a leaf preimage `(d ‖ b)`, a Merkle path, a position, `nk` and `ψ` such that
//!   leaf   = H_LEAF(d ‖ b)                    (the note commitment cm)
//!   anchor = MerkleRoot(leaf, pos, path)      (public)
//!   nf     = H_NF(H_MK(nk ‖ ψ) ‖ pos_lo16 ‖ pos_hi16)   (public)"
//!
//! One Poseidon2-24 permutation per row (the stock Plonky3 Poseidon2 AIR on
//! columns `0..P2`), plus a few control columns. The chaining between hashes
//! is enforced with transition constraints selected by periodic columns.
//! Periodic columns are computed by the verifier and cost nothing in the proof.
//!
//! Row map (rows ≥ 40 are padding: valid permutations, no other constraint):
//!
//! | rows   | what                                  |
//! |--------|---------------------------------------|
//! | 0..4   | leaf sponge over `d ‖ b` (40 elements) |
//! | 4..36  | Merkle compressions, levels 0..31     |
//! | 36..38 | mk sponge over `nk ‖ ψ`               |
//! | 38..40 | nf sponge over `mk ‖ pos_lo ‖ pos_hi` |
//!
//! M4 binds `d ‖ b`, `nk` and `ψ` to the rest of the statement through buses.
//! Here they are free witnesses, so M1 proves membership and the nullifier.

use std::borrow::Cow;

use p3_air::{Air, AirBuilder, BaseAir, WindowAccess};
use p3_field::{PrimeCharacteristicRing, PrimeField32};
use p3_koala_bear::{
    GenericPoseidon2LinearLayersKoalaBear, KOALABEAR_POSEIDON2_RC_24_EXTERNAL_FINAL,
    KOALABEAR_POSEIDON2_RC_24_EXTERNAL_INITIAL, KOALABEAR_POSEIDON2_RC_24_INTERNAL,
};
use p3_matrix::dense::RowMajorMatrix;
use p3_matrix::Matrix;
use p3_poseidon2_air::{generate_trace_rows, num_cols, Poseidon2Air, RoundConstants};
use p3_uni_stark::SubAirBuilder;

use shielded_notes_spike::primitives::{dom, p2_permute, Digest, CHAIN_ID, DIGEST, F, RATE, WIDTH as SW};
use shielded_notes_spike::tree::DEPTH;

const SBOX_DEGREE: u64 = 3;
const SBOX_REGISTERS: usize = 0;
const HALF_FULL: usize = 4;
const PARTIAL: usize = 23;

pub type P2Air =
    Poseidon2Air<F, GenericPoseidon2LinearLayersKoalaBear, SW, SBOX_DEGREE, SBOX_REGISTERS, HALF_FULL, PARTIAL>;

/// Poseidon2 columns (one permutation). The input state is the first 24 columns
/// and the output state the last 24 (the final full round's `post`).
pub const P2: usize = num_cols::<SW, SBOX_DEGREE, SBOX_REGISTERS, HALF_FULL, PARTIAL>();
pub const OUT: usize = P2 - SW;
pub const CHUNK: usize = P2;
pub const BIT: usize = CHUNK + RATE;
pub const ACC_LO: usize = BIT + 1;
pub const ACC_HI: usize = ACC_LO + 1;
pub const COLS: usize = ACC_HI + 1;

pub const LEAF_LEN: usize = 40; // d (12) ‖ b (28)
const LEAF0: usize = 0;
const MERKLE0: usize = 4;
const MK0: usize = MERKLE0 + DEPTH; // 36
const NF0: usize = MK0 + 2; // 38
pub const ROOT_ROW: usize = MK0 - 1; // 35
pub const NF_ROW: usize = NF0 + 1; // 39
pub const USED_ROWS: usize = NF0 + 2; // 40

// Periodic column indices.
mod p {
    pub const CAP_IV: usize = 0; // capacity = IV(dom, len)
    pub const DOM: usize = 1;
    pub const LEN: usize = 2;
    pub const RATE_IS_CHUNK: usize = 3; // rate = chunk (sponge start, absorbing a witness chunk)
    pub const NEXT_CONT: usize = 4; // next row continues this sponge
    pub const NEXT_MERKLE: usize = 5; // next row is a Merkle level fed by this output
    pub const NEXT_FROM_PREV: usize = 6; // next row's rate = this output (new sponge)
    pub const MERKLE: usize = 7; // this row is a Merkle level
    pub const W_LO_NEXT: usize = 8; // weight of next row's bit in pos_lo16
    pub const W_HI_NEXT: usize = 9; // weight of next row's bit in pos_hi16
    pub const ZERO_TAIL4: usize = 10; // chunk[4..] = 0 (last leaf chunk)
    pub const ZERO_ALL: usize = 11; // chunk = 0
    pub const POS_CHUNK: usize = 12; // chunk = (acc_lo, acc_hi, 0, …)
    pub const ROOT: usize = 13; // out[..12] = anchor
    pub const NF: usize = 14; // out[..12] = nf
    pub const COUNT: usize = 15;
}

pub fn round_constants() -> RoundConstants<F, SW, HALF_FULL, PARTIAL> {
    RoundConstants::new(
        KOALABEAR_POSEIDON2_RC_24_EXTERNAL_INITIAL,
        KOALABEAR_POSEIDON2_RC_24_INTERNAL,
        KOALABEAR_POSEIDON2_RC_24_EXTERNAL_FINAL,
    )
}

pub struct InputAir {
    pub height: usize,
    p2: P2Air,
    periodic: Vec<Vec<F>>,
}

impl InputAir {
    pub fn new(height: usize) -> Self {
        assert!(height.is_power_of_two() && height >= 64);
        let mut c = vec![vec![F::ZERO; height]; p::COUNT];
        let mut set = |col: usize, row: usize, v: u32| c[col][row] = F::new(v);
        for (row, d, len) in [(LEAF0, dom::LEAF, LEAF_LEN), (MK0, dom::MK, 24), (NF0, dom::NF, DIGEST + 2)] {
            set(p::CAP_IV, row, 1);
            set(p::DOM, row, d);
            set(p::LEN, row, len as u32);
        }
        set(p::RATE_IS_CHUNK, LEAF0, 1);
        set(p::RATE_IS_CHUNK, MK0, 1);
        for r in [LEAF0, LEAF0 + 1, LEAF0 + 2, MK0, NF0] {
            set(p::NEXT_CONT, r, 1);
        }
        for r in (MERKLE0 - 1)..(MK0 - 1) {
            set(p::NEXT_MERKLE, r, 1);
        }
        set(p::NEXT_FROM_PREV, MK0 + 1, 1);
        for k in 0..DEPTH {
            set(p::MERKLE, MERKLE0 + k, 1);
            let w = 1u32 << (k % 16);
            set(if k < 16 { p::W_LO_NEXT } else { p::W_HI_NEXT }, MERKLE0 + k - 1, w);
        }
        set(p::ZERO_TAIL4, LEAF0 + 3, 1);
        set(p::ZERO_ALL, NF0, 1);
        set(p::POS_CHUNK, NF_ROW, 1);
        set(p::ROOT, ROOT_ROW, 1);
        set(p::NF, NF_ROW, 1);
        InputAir { height, p2: P2Air::new(round_constants()), periodic: c }
    }
}

impl BaseAir<F> for InputAir {
    fn width(&self) -> usize {
        COLS
    }
    fn num_public_values(&self) -> usize {
        2 * DIGEST // anchor ‖ nf
    }
    fn num_periodic_columns(&self) -> usize {
        p::COUNT
    }
    fn periodic_columns(&self) -> Cow<'_, [Vec<F>]> {
        Cow::Borrowed(&self.periodic)
    }
    fn main_next_row_columns(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..SW).collect();
        v.extend(CHUNK..COLS);
        v
    }
    fn max_constraint_degree(&self) -> Option<usize> {
        Some(3)
    }
}

impl<AB: AirBuilder<F = F>> Air<AB> for InputAir {
    fn eval(&self, builder: &mut AB) {
        // Every row is a valid Poseidon2-24 permutation of its input columns.
        {
            let mut sub = SubAirBuilder::<AB, P2Air, AB::Var>::new(builder, 0..P2);
            self.p2.eval(&mut sub);
        }

        let main = builder.main();
        let (lc, nx) = (main.current_slice().to_vec(), main.next_slice().to_vec());
        let pv: Vec<AB::Expr> = builder.public_values().iter().map(|&x| x.into()).collect();
        let per: Vec<AB::Expr> = builder.periodic_values().iter().map(|&x| x.into()).collect();
        let e = |v: AB::Var| -> AB::Expr { v.into() };
        let inp = |r: &[AB::Var], i: usize| e(r[i]);
        let out = |r: &[AB::Var], i: usize| e(r[OUT + i]);
        let chunk = |r: &[AB::Var], i: usize| e(r[CHUNK + i]);

        // Sponge start: capacity = IV(dom, CHAIN_ID, len, 0…). Domain separation.
        let iv = [per[p::DOM].clone(), AB::Expr::from_u32(CHAIN_ID), per[p::LEN].clone()];
        for i in 0..RATE {
            let want = if i < 3 { iv[i].clone() } else { AB::Expr::ZERO };
            builder.assert_zero(per[p::CAP_IV].clone() * (inp(&lc, RATE + i) - want));
        }
        // Sponge start absorbing a witness chunk: rate = chunk.
        for i in 0..RATE {
            builder.assert_zero(per[p::RATE_IS_CHUNK].clone() * (inp(&lc, i) - chunk(&lc, i)));
        }

        {
            let mut t = builder.when_transition();
            // Sponge continuation: rate += chunk, capacity carried.
            for i in 0..RATE {
                t.assert_zero(per[p::NEXT_CONT].clone() * (inp(&nx, i) - out(&lc, i) - chunk(&nx, i)));
                t.assert_zero(per[p::NEXT_CONT].clone() * (inp(&nx, RATE + i) - out(&lc, RATE + i)));
            }
            // Merkle level: (cur, sib) ordered by the direction bit; cur = previous output.
            let b = e(nx[BIT]);
            for i in 0..RATE {
                let (cur, sib) = (out(&lc, i), chunk(&nx, i));
                let left = cur.clone() + b.clone() * (sib.clone() - cur.clone());
                let right = sib.clone() + b.clone() * (cur - sib);
                t.assert_zero(per[p::NEXT_MERKLE].clone() * (inp(&nx, i) - left));
                t.assert_zero(per[p::NEXT_MERKLE].clone() * (inp(&nx, RATE + i) - right));
            }
            // New sponge whose rate is the previous digest (nf over mk).
            for i in 0..RATE {
                t.assert_zero(per[p::NEXT_FROM_PREV].clone() * (inp(&nx, i) - out(&lc, i)));
            }
            // Position accumulators: pos = Σ bit_k·2^k, as two 16-bit limbs. These are
            // the same bits that order the Merkle path, so nf binds the proven position.
            t.assert_zero(e(nx[ACC_LO]) - e(lc[ACC_LO]) - per[p::W_LO_NEXT].clone() * e(nx[BIT]));
            t.assert_zero(e(nx[ACC_HI]) - e(lc[ACC_HI]) - per[p::W_HI_NEXT].clone() * e(nx[BIT]));
        }

        // Bits are boolean on Merkle rows and zero elsewhere (no free bits).
        builder.assert_zero(per[p::MERKLE].clone() * e(lc[BIT]) * (e(lc[BIT]) - AB::Expr::ONE));
        builder.assert_zero((AB::Expr::ONE - per[p::MERKLE].clone()) * e(lc[BIT]));
        builder.when_first_row().assert_zero(e(lc[ACC_LO]));
        builder.when_first_row().assert_zero(e(lc[ACC_HI]));

        // Padding of partial chunks must be zero (canonical sponge input).
        for i in 4..RATE {
            builder.assert_zero(per[p::ZERO_TAIL4].clone() * chunk(&lc, i));
        }
        for i in 0..RATE {
            builder.assert_zero(per[p::ZERO_ALL].clone() * chunk(&lc, i));
        }
        builder.assert_zero(per[p::POS_CHUNK].clone() * (chunk(&lc, 0) - e(lc[ACC_LO])));
        builder.assert_zero(per[p::POS_CHUNK].clone() * (chunk(&lc, 1) - e(lc[ACC_HI])));
        for i in 2..RATE {
            builder.assert_zero(per[p::POS_CHUNK].clone() * chunk(&lc, i));
        }

        // Public outputs.
        for i in 0..DIGEST {
            builder.assert_zero(per[p::ROOT].clone() * (out(&lc, i) - pv[i].clone()));
            builder.assert_zero(per[p::NF].clone() * (out(&lc, i) - pv[DIGEST + i].clone()));
        }
    }
}

/// Witness for one input.
#[derive(Clone)]
pub struct InputWitness {
    pub leaf_preimage: [F; LEAF_LEN],
    pub path: [Digest; DEPTH],
    pub pos: u64,
    pub nk: Digest,
    pub psi: Digest,
}

/// Build the trace and public values `(anchor ‖ nf)`, computed by the same
/// row-by-row chaining the AIR enforces.
pub fn build_trace(w: &InputWitness, height: usize) -> (RowMajorMatrix<F>, Vec<F>) {
    let mut inputs = vec![[F::ZERO; SW]; height];
    let mut chunks = vec![[F::ZERO; RATE]; height];
    let mut bits = vec![F::ZERO; height];
    let iv = |d: u32, len: usize| {
        let mut c = [F::ZERO; RATE];
        c[0] = F::new(d);
        c[1] = F::new(CHAIN_ID);
        c[2] = F::new(len as u32);
        c
    };
    let mut outs = vec![[F::ZERO; SW]; height];
    let run = |row: usize, input: [F; SW], outs: &mut Vec<[F; SW]>, inputs: &mut Vec<[F; SW]>| {
        inputs[row] = input;
        let mut s = input;
        p2_permute(&mut s);
        outs[row] = s;
    };
    // Leaf sponge.
    for c in 0..4 {
        let mut ch = [F::ZERO; RATE];
        let n = (LEAF_LEN - 12 * c).min(12);
        ch[..n].copy_from_slice(&w.leaf_preimage[12 * c..12 * c + n]);
        chunks[LEAF0 + c] = ch;
        let mut input = [F::ZERO; SW];
        if c == 0 {
            input[..RATE].copy_from_slice(&ch);
            input[RATE..].copy_from_slice(&iv(dom::LEAF, LEAF_LEN));
        } else {
            let prev = outs[LEAF0 + c - 1];
            for i in 0..RATE {
                input[i] = prev[i] + ch[i];
            }
            input[RATE..].copy_from_slice(&prev[RATE..]);
        }
        run(LEAF0 + c, input, &mut outs, &mut inputs);
    }
    // Merkle levels.
    for k in 0..DEPTH {
        let row = MERKLE0 + k;
        let bit = (w.pos >> k) & 1;
        bits[row] = F::new(bit as u32);
        chunks[row] = w.path[k];
        let cur: [F; RATE] = outs[row - 1][..RATE].try_into().unwrap();
        let (l, r) = if bit == 0 { (cur, w.path[k]) } else { (w.path[k], cur) };
        let mut input = [F::ZERO; SW];
        input[..RATE].copy_from_slice(&l);
        input[RATE..].copy_from_slice(&r);
        run(row, input, &mut outs, &mut inputs);
    }
    // mk sponge.
    for (c, part) in [w.nk, w.psi].iter().enumerate() {
        let row = MK0 + c;
        chunks[row] = *part;
        let mut input = [F::ZERO; SW];
        if c == 0 {
            input[..RATE].copy_from_slice(part);
            input[RATE..].copy_from_slice(&iv(dom::MK, 24));
        } else {
            let prev = outs[row - 1];
            for i in 0..RATE {
                input[i] = prev[i] + part[i];
            }
            input[RATE..].copy_from_slice(&prev[RATE..]);
        }
        run(row, input, &mut outs, &mut inputs);
    }
    // nf sponge: rate = mk (previous digest), then (pos_lo, pos_hi).
    let (lo, hi) = (F::new((w.pos & 0xffff) as u32), F::new(((w.pos >> 16) & 0xffff) as u32));
    {
        let mut input = [F::ZERO; SW];
        input[..RATE].copy_from_slice(&outs[NF0 - 1][..RATE]);
        input[RATE..].copy_from_slice(&iv(dom::NF, DIGEST + 2));
        run(NF0, input, &mut outs, &mut inputs);
        let mut ch = [F::ZERO; RATE];
        ch[0] = lo;
        ch[1] = hi;
        chunks[NF_ROW] = ch;
        let prev = outs[NF0];
        let mut input = [F::ZERO; SW];
        for i in 0..RATE {
            input[i] = prev[i] + ch[i];
        }
        input[RATE..].copy_from_slice(&prev[RATE..]);
        run(NF_ROW, input, &mut outs, &mut inputs);
    }

    let p2 = generate_trace_rows::<F, GenericPoseidon2LinearLayersKoalaBear, SW, SBOX_DEGREE, SBOX_REGISTERS, HALF_FULL, PARTIAL>(
        inputs,
        &round_constants(),
        0,
    );
    let mut values = Vec::with_capacity(height * COLS);
    let (mut acc_lo, mut acc_hi) = (F::ZERO, F::ZERO);
    for row in 0..height {
        if (MERKLE0..MK0).contains(&row) {
            let k = row - MERKLE0;
            let w = F::new(1 << (k % 16));
            if k < 16 { acc_lo += w * bits[row] } else { acc_hi += w * bits[row] }
        }
        values.extend_from_slice(&p2.row_slice(row).unwrap());
        values.extend_from_slice(&chunks[row]);
        values.push(bits[row]);
        values.push(acc_lo);
        values.push(acc_hi);
    }
    let trace = RowMajorMatrix::new(values, COLS);
    // The Plonky3 trace generator and the native permutation must agree.
    for row in 0..USED_ROWS {
        let got: Vec<u32> = trace.row_slice(row).unwrap()[OUT..P2].iter().map(|x| x.as_canonical_u32()).collect();
        let want: Vec<u32> = outs[row].iter().map(|x| x.as_canonical_u32()).collect();
        assert_eq!(got, want, "AIR permutation != native permutation at row {row}");
    }
    let mut pv = outs[ROOT_ROW][..DIGEST].to_vec();
    pv.extend_from_slice(&outs[NF_ROW][..DIGEST]);
    (trace, pv)
}

impl crate::stark::ProvableAir for InputAir {
    fn trace_height(&self) -> usize {
        self.height
    }
    fn air_id(&self) -> &'static str {
        "mazze/shielded/m1-input/v1"
    }
}
