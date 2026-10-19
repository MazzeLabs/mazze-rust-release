//! M1.5: the same statement as `input_air` (leaf + 32-level Merkle + mk + nf),
//! laid out **one Poseidon2 round per row** instead of one permutation per row.
//!
//! - One permutation occupies 32 rows: row 0 holds `M_E(input)` (the initial
//!   external layer applied), and rows 1..=31 hold the state after rounds
//!   0..=30. Row 31 is the permutation output.
//! - Columns: state (24) ‖ chunk (12) ‖ bit ‖ acc_lo ‖ acc_hi = 39, against
//!   254 for one-permutation-per-row. Round constants and all selectors are
//!   periodic columns, computed by the verifier and absent from the proof.
//! - Round transition (rows r → r+1, r = 0..30):
//!     full:    next = M_E((s + rc)^3)                  (all 24 lanes)
//!     partial: next = M_I((s0 + rc0)^3, s1, …, s23)
//!   degree 4 (periodic selector × cube).
//! - Permutation boundary (row 31 of perm k−1 → row 0 of perm k): next row 0 =
//!   M_E(input_k), with `input_k` built from the previous output, the chunk and
//!   the direction bit, exactly as in `input_air`. Degree 3.
//! - Perm 0 is a dummy, so every real permutation's input is a transition
//!   constraint.

use std::borrow::Cow;

use p3_air::{Air, AirBuilder, BaseAir, WindowAccess};
use p3_field::{PrimeCharacteristicRing, PrimeField32};
use p3_koala_bear::{
    GenericPoseidon2LinearLayersKoalaBear as LL, KOALABEAR_POSEIDON2_RC_24_EXTERNAL_FINAL as RC_END,
    KOALABEAR_POSEIDON2_RC_24_EXTERNAL_INITIAL as RC_BEGIN, KOALABEAR_POSEIDON2_RC_24_INTERNAL as RC_PART,
};
use p3_matrix::dense::RowMajorMatrix;
use p3_poseidon2::GenericPoseidon2LinearLayers;

use shielded_notes_spike::primitives::{dom, p2_permute, CHAIN_ID, DIGEST, F, RATE, WIDTH as SW};
use shielded_notes_spike::tree::DEPTH;

use crate::input_air::{InputWitness, LEAF_LEN};

pub const ROWS_PER_PERM: usize = 32;
pub const S: usize = 0;
pub const CHUNK: usize = SW;
pub const BIT: usize = CHUNK + RATE;
pub const ACC_LO: usize = BIT + 1;
pub const ACC_HI: usize = ACC_LO + 1;
pub const COLS: usize = ACC_HI + 1;

const fn is_full(r: usize) -> bool {
    r < 4 || (r >= 27 && r < 31)
}

/// How a permutation's input is formed (spec sponge/compression modes).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// rate = chunk, capacity = IV(dom, len)
    StartChunk { dom: u32, len: u32 },
    /// rate = prev.rate + chunk, capacity = prev.capacity
    Cont,
    /// (cur, sib) ordered by the bit, cur = prev.rate
    Merkle,
    /// rate = prev.rate (a digest), capacity = IV(dom, len)
    StartFromPrev { dom: u32, len: u32 },
}

/// The M1 statement's permutation schedule. Index 0 is the dummy.
pub fn schedule() -> Vec<Option<Kind>> {
    let mut v = vec![None];
    v.push(Some(Kind::StartChunk { dom: dom::LEAF, len: LEAF_LEN as u32 }));
    v.extend([Some(Kind::Cont); 3]);
    v.extend([Some(Kind::Merkle); DEPTH]);
    v.push(Some(Kind::StartChunk { dom: dom::MK, len: 24 }));
    v.push(Some(Kind::Cont));
    v.push(Some(Kind::StartFromPrev { dom: dom::NF, len: (DIGEST + 2) as u32 }));
    v.push(Some(Kind::Cont));
    v
}
const LEAF_LAST: usize = 4;
const MERKLE_FIRST: usize = 5;
pub const ROOT_PERM: usize = MERKLE_FIRST + DEPTH - 1; // 36
const MK_FIRST: usize = ROOT_PERM + 1; // 37
const NF_FIRST: usize = MK_FIRST + 2; // 39
pub const NF_PERM: usize = NF_FIRST + 1; // 40

mod p {
    // Period 32 (round structure).
    pub const IS_FULL: usize = 0;
    pub const IS_PART: usize = 1;
    pub const RC: usize = 2; // 24 columns
    // Period = height (statement layout).
    pub const SEL_START_CHUNK: usize = 26;
    pub const SEL_CONT: usize = 27;
    pub const SEL_MERKLE: usize = 28;
    pub const SEL_FROM_PREV: usize = 29;
    pub const IV_DOM: usize = 30;
    pub const IV_LEN: usize = 31;
    pub const MERKLE_BIT_ROW: usize = 32;
    pub const W_LO_NEXT: usize = 33;
    pub const W_HI_NEXT: usize = 34;
    pub const ZERO_TAIL4: usize = 35;
    pub const ZERO_ALL: usize = 36;
    pub const POS_CHUNK: usize = 37;
    pub const ROOT: usize = 38;
    pub const NF: usize = 39;
    pub const COUNT: usize = 40;
}

pub struct NarrowAir {
    pub height: usize,
    periodic: Vec<Vec<F>>,
}

impl NarrowAir {
    pub fn new(height: usize) -> Self {
        let sched = schedule();
        assert!(height.is_power_of_two() && height >= sched.len() * ROWS_PER_PERM);
        let mut c: Vec<Vec<F>> = (0..p::COUNT)
            .map(|i| vec![F::ZERO; if i < p::SEL_START_CHUNK { ROWS_PER_PERM } else { height }])
            .collect();
        for r in 0..ROWS_PER_PERM - 1 {
            if is_full(r) {
                c[p::IS_FULL][r] = F::ONE;
                let rc = if r < 4 { RC_BEGIN[r] } else { RC_END[r - 27] };
                for i in 0..SW {
                    c[p::RC + i][r] = rc[i];
                }
            } else {
                c[p::IS_PART][r] = F::ONE;
                c[p::RC][r] = RC_PART[r - 4];
            }
        }
        for (k, kind) in sched.iter().enumerate().skip(1) {
            let boundary = k * ROWS_PER_PERM - 1; // row 31 of perm k−1
            let row0 = k * ROWS_PER_PERM;
            match kind.unwrap() {
                Kind::StartChunk { dom, len } => {
                    c[p::SEL_START_CHUNK][boundary] = F::ONE;
                    c[p::IV_DOM][boundary] = F::new(dom);
                    c[p::IV_LEN][boundary] = F::new(len);
                }
                Kind::Cont => c[p::SEL_CONT][boundary] = F::ONE,
                Kind::Merkle => {
                    c[p::SEL_MERKLE][boundary] = F::ONE;
                    c[p::MERKLE_BIT_ROW][row0] = F::ONE;
                    let level = k - MERKLE_FIRST;
                    let w = F::new(1 << (level % 16));
                    c[if level < 16 { p::W_LO_NEXT } else { p::W_HI_NEXT }][boundary] = w;
                }
                Kind::StartFromPrev { dom, len } => {
                    c[p::SEL_FROM_PREV][boundary] = F::ONE;
                    c[p::IV_DOM][boundary] = F::new(dom);
                    c[p::IV_LEN][boundary] = F::new(len);
                }
            }
        }
        c[p::ZERO_TAIL4][LEAF_LAST * ROWS_PER_PERM] = F::ONE;
        c[p::ZERO_ALL][NF_FIRST * ROWS_PER_PERM] = F::ONE;
        c[p::POS_CHUNK][NF_PERM * ROWS_PER_PERM] = F::ONE;
        c[p::ROOT][ROOT_PERM * ROWS_PER_PERM + 31] = F::ONE;
        c[p::NF][NF_PERM * ROWS_PER_PERM + 31] = F::ONE;
        NarrowAir { height, periodic: c }
    }
}

impl BaseAir<F> for NarrowAir {
    fn width(&self) -> usize {
        COLS
    }
    fn num_public_values(&self) -> usize {
        2 * DIGEST
    }
    fn num_periodic_columns(&self) -> usize {
        p::COUNT
    }
    fn periodic_columns(&self) -> Cow<'_, [Vec<F>]> {
        Cow::Borrowed(&self.periodic)
    }
    fn main_next_row_columns(&self) -> Vec<usize> {
        (0..COLS).collect()
    }
    fn max_constraint_degree(&self) -> Option<usize> {
        Some(4)
    }
}

impl<AB: AirBuilder<F = F>> Air<AB> for NarrowAir {
    fn eval(&self, builder: &mut AB) {
        let main = builder.main();
        let (lc, nx) = (main.current_slice().to_vec(), main.next_slice().to_vec());
        let pv: Vec<AB::Expr> = builder.public_values().iter().map(|&x| x.into()).collect();
        let per: Vec<AB::Expr> = builder.periodic_values().iter().map(|&x| x.into()).collect();
        let e = |v: AB::Var| -> AB::Expr { v.into() };

        {
            let mut t = builder.when_transition();

            // --- Poseidon2 rounds inside a permutation ---
            let mut full: [AB::Expr; SW] = std::array::from_fn(|i| {
                let x = e(lc[S + i]) + per[p::RC + i].clone();
                x.clone() * x.clone() * x
            });
            LL::external_linear_layer(&mut full);
            let mut part: [AB::Expr; SW] = std::array::from_fn(|i| {
                if i == 0 {
                    let x = e(lc[S]) + per[p::RC].clone();
                    x.clone() * x.clone() * x
                } else {
                    e(lc[S + i])
                }
            });
            LL::internal_linear_layer(&mut part);
            let in_round = per[p::IS_FULL].clone() + per[p::IS_PART].clone();
            for i in 0..SW {
                t.assert_zero(
                    in_round.clone() * e(nx[S + i])
                        - per[p::IS_FULL].clone() * full[i].clone()
                        - per[p::IS_PART].clone() * part[i].clone(),
                );
            }

            // --- Permutation boundary: next row 0 = M_E(input) ---
            let (sc, sco, sm, sf) = (
                per[p::SEL_START_CHUNK].clone(),
                per[p::SEL_CONT].clone(),
                per[p::SEL_MERKLE].clone(),
                per[p::SEL_FROM_PREV].clone(),
            );
            let b = e(nx[BIT]);
            let iv = |i: usize| -> AB::Expr {
                match i {
                    0 => per[p::IV_DOM].clone(),
                    1 => AB::Expr::from_u32(CHAIN_ID),
                    2 => per[p::IV_LEN].clone(),
                    _ => AB::Expr::ZERO,
                }
            };
            let mut input: [AB::Expr; SW] = std::array::from_fn(|j| {
                if j < RATE {
                    let (out, ch) = (e(lc[S + j]), e(nx[CHUNK + j]));
                    sc.clone() * ch.clone()
                        + sco.clone() * (out.clone() + ch.clone())
                        + sm.clone() * (out.clone() + b.clone() * (ch - out.clone()))
                        + sf.clone() * out
                } else {
                    let i = j - RATE;
                    let (cur, sib) = (e(lc[S + i]), e(nx[CHUNK + i]));
                    (sc.clone() + sf.clone()) * iv(i)
                        + sco.clone() * e(lc[S + j])
                        + sm.clone() * (sib.clone() + b.clone() * (cur - sib))
                }
            });
            LL::external_linear_layer(&mut input);
            let any = sc + sco + sm + sf;
            for j in 0..SW {
                t.assert_zero(any.clone() * e(nx[S + j]) - input[j].clone());
            }

            // --- Position: the bits ordering the path, as two 16-bit limbs ---
            t.assert_zero(e(nx[ACC_LO]) - e(lc[ACC_LO]) - per[p::W_LO_NEXT].clone() * e(nx[BIT]));
            t.assert_zero(e(nx[ACC_HI]) - e(lc[ACC_HI]) - per[p::W_HI_NEXT].clone() * e(nx[BIT]));
        }

        let bit = e(lc[BIT]);
        builder.assert_zero(per[p::MERKLE_BIT_ROW].clone() * bit.clone() * (bit.clone() - AB::Expr::ONE));
        builder.assert_zero((AB::Expr::ONE - per[p::MERKLE_BIT_ROW].clone()) * bit);
        builder.when_first_row().assert_zero(e(lc[ACC_LO]));
        builder.when_first_row().assert_zero(e(lc[ACC_HI]));

        for i in 4..RATE {
            builder.assert_zero(per[p::ZERO_TAIL4].clone() * e(lc[CHUNK + i]));
        }
        for i in 0..RATE {
            builder.assert_zero(per[p::ZERO_ALL].clone() * e(lc[CHUNK + i]));
        }
        builder.assert_zero(per[p::POS_CHUNK].clone() * (e(lc[CHUNK]) - e(lc[ACC_LO])));
        builder.assert_zero(per[p::POS_CHUNK].clone() * (e(lc[CHUNK + 1]) - e(lc[ACC_HI])));
        for i in 2..RATE {
            builder.assert_zero(per[p::POS_CHUNK].clone() * e(lc[CHUNK + i]));
        }
        for i in 0..DIGEST {
            builder.assert_zero(per[p::ROOT].clone() * (e(lc[S + i]) - pv[i].clone()));
            builder.assert_zero(per[p::NF].clone() * (e(lc[S + i]) - pv[DIGEST + i].clone()));
        }
    }
}

impl crate::stark::ProvableAir for NarrowAir {
    fn trace_height(&self) -> usize {
        self.height
    }
    fn air_id(&self) -> &'static str {
        "mazze/shielded/m15-input/v1"
    }
}

/// The 32 row states of one permutation: `M_E(input)`, then after each round.
fn perm_rows(input: [F; SW]) -> [[F; SW]; ROWS_PER_PERM] {
    let mut rows = [[F::ZERO; SW]; ROWS_PER_PERM];
    let mut st = input;
    LL::external_linear_layer(&mut st);
    rows[0] = st;
    for r in 0..ROWS_PER_PERM - 1 {
        if is_full(r) {
            let rc = if r < 4 { RC_BEGIN[r] } else { RC_END[r - 27] };
            for i in 0..SW {
                let x = st[i] + rc[i];
                st[i] = x * x * x;
            }
            LL::external_linear_layer(&mut st);
        } else {
            let x = st[0] + RC_PART[r - 4];
            st[0] = x * x * x;
            LL::internal_linear_layer(&mut st);
        }
        rows[r + 1] = st;
    }
    rows
}

pub fn build_trace(w: &InputWitness, height: usize) -> (RowMajorMatrix<F>, Vec<F>) {
    let sched = schedule();
    let n = sched.len();
    let mut chunks = vec![[F::ZERO; RATE]; n];
    let mut bits = vec![F::ZERO; n];
    for c in 0..4 {
        let len = (LEAF_LEN - 12 * c).min(12);
        chunks[1 + c][..len].copy_from_slice(&w.leaf_preimage[12 * c..12 * c + len]);
    }
    for k in 0..DEPTH {
        chunks[MERKLE_FIRST + k] = w.path[k];
        bits[MERKLE_FIRST + k] = F::new(((w.pos >> k) & 1) as u32);
    }
    chunks[MK_FIRST] = w.nk;
    chunks[MK_FIRST + 1] = w.psi;
    chunks[NF_PERM][0] = F::new((w.pos & 0xffff) as u32);
    chunks[NF_PERM][1] = F::new(((w.pos >> 16) & 0xffff) as u32);

    let iv = |d: u32, len: u32| {
        let mut c = [F::ZERO; RATE];
        c[0] = F::new(d);
        c[1] = F::new(CHAIN_ID);
        c[2] = F::new(len);
        c
    };
    let mut perm_inputs = vec![[F::ZERO; SW]; height / ROWS_PER_PERM];
    let mut outs = vec![[F::ZERO; SW]; height / ROWS_PER_PERM];
    {
        let mut o = [F::ZERO; SW];
        p2_permute(&mut o);
        outs[0] = o; // dummy perm of the zero state
    }
    for k in 1..n {
        let prev = outs[k - 1];
        let ch = chunks[k];
        let mut input = [F::ZERO; SW];
        match sched[k].unwrap() {
            Kind::StartChunk { dom, len } => {
                input[..RATE].copy_from_slice(&ch);
                input[RATE..].copy_from_slice(&iv(dom, len));
            }
            Kind::Cont => {
                for i in 0..RATE {
                    input[i] = prev[i] + ch[i];
                }
                input[RATE..].copy_from_slice(&prev[RATE..]);
            }
            Kind::Merkle => {
                let cur = &prev[..RATE];
                let (l, r) = if bits[k] == F::ZERO { (cur, &ch[..]) } else { (&ch[..], cur) };
                input[..RATE].copy_from_slice(l);
                input[RATE..].copy_from_slice(r);
            }
            Kind::StartFromPrev { dom, len } => {
                input[..RATE].copy_from_slice(&prev[..RATE]);
                input[RATE..].copy_from_slice(&iv(dom, len));
            }
        }
        perm_inputs[k] = input;
        let mut o = input;
        p2_permute(&mut o);
        outs[k] = o;
    }
    for k in n..perm_inputs.len() {
        let mut o = [F::ZERO; SW];
        p2_permute(&mut o);
        outs[k] = o;
    }

    let mut values = Vec::with_capacity(height * COLS);
    let (mut acc_lo, mut acc_hi) = (F::ZERO, F::ZERO);
    for (k, input) in perm_inputs.iter().enumerate() {
        let rows = perm_rows(*input);
        assert_eq!(
            rows[31].map(|x| x.as_canonical_u32()),
            outs[k].map(|x| x.as_canonical_u32()),
            "round-by-round state != native permutation at perm {k}"
        );
        let (ch, bit) = if k < n { (chunks[k], bits[k]) } else { ([F::ZERO; RATE], F::ZERO) };
        if (MERKLE_FIRST..MERKLE_FIRST + DEPTH).contains(&k) {
            let level = k - MERKLE_FIRST;
            let wgt = F::new(1 << (level % 16));
            if level < 16 { acc_lo += wgt * bit } else { acc_hi += wgt * bit }
        }
        for (r, st) in rows.iter().enumerate() {
            values.extend_from_slice(st);
            // chunk and bit live on row 0 of the permutation they feed.
            if r == 0 { values.extend_from_slice(&ch); values.push(bit); } else { values.extend([F::ZERO; RATE]); values.push(F::ZERO); }
            values.push(acc_lo);
            values.push(acc_hi);
        }
    }
    let mut pv = outs[ROOT_PERM][..DIGEST].to_vec();
    pv.extend_from_slice(&outs[NF_PERM][..DIGEST]);
    (RowMajorMatrix::new(values, COLS), pv)
}
