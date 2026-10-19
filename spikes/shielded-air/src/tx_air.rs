//! M2: one input bound end to end, plus one output, in the narrow layout
//! (one Poseidon2 round per row, 32 rows per permutation).
//!
//! Input:  nsk → nk = H_NK(nsk), ak = H_AK(nsk), owner = H_OWN(ak ‖ nk ‖ d)
//!         x = owner ‖ limbs16(value) ‖ ψ,  cm = H_LEAF(H_HM(y) ‖ x − A·y)
//!         anchor = Root(cm, pos, path),    nf = H_NF(H_MK(nk ‖ ψ) ‖ pos)
//! Output: x' = owner' ‖ limbs16(value') ‖ ψ',  cm_out = H_LEAF(H_HM(y') ‖ x' − A·y')
//! Public: anchor ‖ nf ‖ cm_out.
//!
//! Binding devices (all constraints, nothing trusted):
//! - **Registers** NK, OWNER, PSI, VAL are columns held constant over a note's
//!   rows (`KEEP_REGS`) and pinned where they're produced (`LOAD_*`) or used
//!   (`EQ_*`, the b-check).
//! - **Carried chunk**: the nk permutation's chunk (= nsk) is carried to the ak
//!   permutation's chunk, so both derive from the same nsk (spec C1, Z-1).
//!   Inside HM permutations the chunk (12 elements of y) is carried down all
//!   32 rows, so A·y can consume one element per row.
//! - **A·y**: 28 accumulators; on the row that consumes y_j,
//!   `AY_i += A[i][j]·y_j`. The coefficients are periodic columns.
//!   At the leaf, `chunk = x − AY` for each b element.
//! Ranges and the balance are M3.

use std::borrow::Cow;

use p3_air::{Air, AirBuilder, BaseAir, WindowAccess};
use p3_field::{PrimeCharacteristicRing, PrimeField32};
use p3_koala_bear::{
    GenericPoseidon2LinearLayersKoalaBear as LL, KOALABEAR_POSEIDON2_RC_24_EXTERNAL_FINAL as RC_END,
    KOALABEAR_POSEIDON2_RC_24_EXTERNAL_INITIAL as RC_BEGIN, KOALABEAR_POSEIDON2_RC_24_INTERNAL as RC_PART,
};
use p3_matrix::dense::RowMajorMatrix;
use p3_poseidon2::GenericPoseidon2LinearLayers;

use shielded_notes_spike::keys::SpendKeys;
use shielded_notes_spike::note::{hm_matrix, Note, HM_L, HM_T};
use shielded_notes_spike::primitives::{dom, limbs, p2_permute, Digest, CHAIN_ID, DIGEST, F, RATE, WIDTH as SW};
use shielded_notes_spike::tree::DEPTH;

pub const R: usize = 32;

// ---- columns ----
pub const S: usize = 0;
pub const CHUNK: usize = SW;
pub const BIT: usize = CHUNK + RATE;
pub const ACC_LO: usize = BIT + 1;
pub const ACC_HI: usize = ACC_LO + 1;
pub const REG_NK: usize = ACC_HI + 1;
pub const REG_OWNER: usize = REG_NK + DIGEST;
pub const REG_PSI: usize = REG_OWNER + DIGEST;
pub const REG_VAL: usize = REG_PSI + DIGEST;
pub const AY: usize = REG_VAL + 4;
pub const COLS: usize = AY + HM_T;
const REGS: std::ops::Range<usize> = REG_NK..AY;

pub(crate) const fn is_full(r: usize) -> bool {
    r < 4 || (r >= 27 && r < 31)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    StartChunk { dom: u32, len: u32 },
    Cont,
    Merkle,
    StartFromPrev { dom: u32, len: u32 },
}

/// Constraint on the chunk at a permutation's row 0.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Chunk {
    Free,
    /// equal to the previous permutation's (carried) chunk
    FromPrev,
    EqNk,
    EqPsi,
    /// chunk[n..] = 0
    PadAfter(usize),
    Zero,
    Pos,
    /// b block `k` (elements 12k..): chunk = x − AY (zero padded)
    B(usize),
}

#[derive(Clone, Copy, Debug)]
pub struct Perm {
    pub kind: Kind,
    pub chunk: Chunk,
    /// chunk is carried down all 32 rows (nk perm: into the ak perm; HM perms: for A·y)
    pub carry: bool,
    /// HM block index m: rows r < len consume y_{12m + r}
    pub hm: Option<usize>,
}

pub mod idx {
    pub const NK: usize = 1;
    pub const AK: usize = 2;
    pub const OWN: usize = 3; // 3, 4, 5
    pub const HM_IN: usize = 6; // 6..12
    pub const LEAF_IN: usize = 12; // 12..16
    pub const MERKLE: usize = 16; // 16..48
    pub const MK: usize = 48; // 48, 49
    pub const NF: usize = 50; // 50, 51
    pub const HM_OUT: usize = 52; // 52..58
    pub const LEAF_OUT: usize = 58; // 58..62
    pub const COUNT: usize = 62;
}

pub fn schedule() -> Vec<Option<Perm>> {
    use Chunk::*;
    use Kind::*;
    let p = |kind, chunk| Some(Perm { kind, chunk, carry: false, hm: None });
    let mut v: Vec<Option<Perm>> = vec![None];
    v.push(Some(Perm { kind: StartChunk { dom: dom::NK, len: 12 }, chunk: Free, carry: true, hm: None }));
    v.push(p(StartChunk { dom: dom::AK, len: 12 }, FromPrev));
    v.push(p(StartFromPrev { dom: dom::OWN, len: 27 }, Zero));
    v.push(p(Cont, EqNk));
    v.push(p(Cont, PadAfter(3)));
    let hm = |v: &mut Vec<Option<Perm>>| {
        for m in 0..6 {
            let kind = if m == 0 { StartChunk { dom: dom::HM_DIGEST, len: HM_L as u32 } } else { Cont };
            let chunk = if m == 5 { PadAfter(HM_L - 60) } else { Free };
            v.push(Some(Perm { kind, chunk, carry: true, hm: Some(m) }));
        }
        v.push(p(StartFromPrev { dom: dom::LEAF, len: 40 }, Zero));
        for k in 0..3 {
            v.push(p(Cont, B(k)));
        }
    };
    hm(&mut v);
    for _ in 0..DEPTH {
        v.push(p(Merkle, Free));
    }
    v.push(p(StartChunk { dom: dom::MK, len: 24 }, EqNk));
    v.push(p(Cont, EqPsi));
    v.push(p(StartFromPrev { dom: dom::NF, len: (DIGEST + 2) as u32 }, Zero));
    v.push(p(Cont, Pos));
    hm(&mut v);
    assert_eq!(v.len(), idx::COUNT);
    v
}

// ---- periodic columns ----
mod pc {
    use super::{HM_T, RATE, SW};
    pub const IS_FULL: usize = 0;
    pub const IS_PART: usize = 1;
    pub const RC: usize = 2;
    pub const ROUND_END: usize = RC + SW; // period-32 columns end here
    pub const SEL_START: usize = ROUND_END;
    pub const SEL_CONT: usize = SEL_START + 1;
    pub const SEL_MERKLE: usize = SEL_START + 2;
    pub const SEL_FROM_PREV: usize = SEL_START + 3;
    pub const IV_DOM: usize = SEL_START + 4;
    pub const IV_LEN: usize = SEL_START + 5;
    pub const BIT_ROW: usize = SEL_START + 6;
    pub const W_LO_NEXT: usize = SEL_START + 7;
    pub const W_HI_NEXT: usize = SEL_START + 8;
    pub const CARRY_NEXT: usize = SEL_START + 9; // transition: next.chunk = chunk
    pub const EQ_NK: usize = SEL_START + 10;
    pub const EQ_PSI: usize = SEL_START + 11;
    pub const PAD3: usize = SEL_START + 12;
    pub const PAD4: usize = SEL_START + 13;
    pub const ZERO: usize = SEL_START + 14;
    pub const POS: usize = SEL_START + 15;
    pub const B: usize = SEL_START + 16; // 3 columns
    pub const LOAD_NK: usize = B + 3;
    pub const LOAD_OWNER: usize = LOAD_NK + 1;
    pub const KEEP_REGS: usize = LOAD_NK + 2; // transition
    pub const AY_RESET: usize = LOAD_NK + 3; // transition
    pub const COEF_NEXT: usize = LOAD_NK + 4; // HM_T columns
    pub const YT_NEXT: usize = COEF_NEXT + HM_T; // RATE columns
    pub const ROOT: usize = YT_NEXT + RATE;
    pub const NF: usize = ROOT + 1;
    pub const CM_OUT: usize = ROOT + 2;
    pub const COUNT: usize = ROOT + 3;
}

pub struct TxAir {
    pub height: usize,
    periodic: Vec<Vec<F>>,
}

impl TxAir {
    pub fn new(height: usize) -> Self {
        let sched = schedule();
        assert!(height.is_power_of_two() && height >= sched.len() * R);
        let a = hm_matrix();
        let mut c: Vec<Vec<F>> =
            (0..pc::COUNT).map(|i| vec![F::ZERO; if i < pc::ROUND_END { R } else { height }]).collect();
        for r in 0..R - 1 {
            if is_full(r) {
                c[pc::IS_FULL][r] = F::ONE;
                let rc = if r < 4 { RC_BEGIN[r] } else { RC_END[r - 27] };
                for i in 0..SW {
                    c[pc::RC + i][r] = rc[i];
                }
            } else {
                c[pc::IS_PART][r] = F::ONE;
                c[pc::RC][r] = RC_PART[r - 4];
            }
        }
        // Registers are held across all transitions except the note boundary.
        for row in 0..height - 1 {
            c[pc::KEEP_REGS][row] = F::ONE;
        }
        c[pc::KEEP_REGS][idx::HM_OUT * R - 1] = F::ZERO;
        let mut merkle_level = 0;
        for (k, perm) in sched.iter().enumerate().skip(1) {
            let perm = perm.unwrap();
            let (prev_end, row0) = (k * R - 1, k * R);
            match perm.kind {
                Kind::StartChunk { dom, len } => {
                    c[pc::SEL_START][prev_end] = F::ONE;
                    c[pc::IV_DOM][prev_end] = F::new(dom);
                    c[pc::IV_LEN][prev_end] = F::new(len);
                }
                Kind::Cont => c[pc::SEL_CONT][prev_end] = F::ONE,
                Kind::Merkle => {
                    c[pc::SEL_MERKLE][prev_end] = F::ONE;
                    c[pc::BIT_ROW][row0] = F::ONE;
                    let w = F::new(1 << (merkle_level % 16));
                    c[if merkle_level < 16 { pc::W_LO_NEXT } else { pc::W_HI_NEXT }][prev_end] = w;
                    merkle_level += 1;
                }
                Kind::StartFromPrev { dom, len } => {
                    c[pc::SEL_FROM_PREV][prev_end] = F::ONE;
                    c[pc::IV_DOM][prev_end] = F::new(dom);
                    c[pc::IV_LEN][prev_end] = F::new(len);
                }
            }
            match perm.chunk {
                Chunk::Free => {}
                Chunk::FromPrev => c[pc::CARRY_NEXT][prev_end] = F::ONE,
                Chunk::EqNk => c[pc::EQ_NK][row0] = F::ONE,
                Chunk::EqPsi => c[pc::EQ_PSI][row0] = F::ONE,
                Chunk::PadAfter(3) => c[pc::PAD3][row0] = F::ONE,
                Chunk::PadAfter(4) => c[pc::PAD4][row0] = F::ONE,
                Chunk::PadAfter(n) => panic!("unsupported pad {n}"),
                Chunk::Zero => c[pc::ZERO][row0] = F::ONE,
                Chunk::Pos => c[pc::POS][row0] = F::ONE,
                Chunk::B(b) => c[pc::B + b][row0] = F::ONE,
            }
            if perm.carry {
                for r in 0..R - 1 {
                    c[pc::CARRY_NEXT][row0 + r] = F::ONE;
                }
            }
            if let Some(m) = perm.hm {
                if m == 0 {
                    c[pc::AY_RESET][prev_end] = F::ONE;
                }
                let n = (HM_L - 12 * m).min(12);
                for r in 0..n {
                    let j = 12 * m + r;
                    let row = row0 + r; // the row consuming y_j
                    for i in 0..HM_T {
                        c[pc::COEF_NEXT + i][row - 1] = a[i][j];
                    }
                    c[pc::YT_NEXT + r][row - 1] = F::ONE;
                }
            }
        }
        c[pc::LOAD_NK][(idx::NK + 1) * R - 1] = F::ONE;
        c[pc::LOAD_OWNER][(idx::OWN + 3) * R - 1] = F::ONE;
        c[pc::ROOT][(idx::MERKLE + DEPTH) * R - 1] = F::ONE;
        c[pc::NF][(idx::NF + 2) * R - 1] = F::ONE;
        c[pc::CM_OUT][idx::COUNT * R - 1] = F::ONE;
        TxAir { height, periodic: c }
    }
}

impl BaseAir<F> for TxAir {
    fn width(&self) -> usize {
        COLS
    }
    fn num_public_values(&self) -> usize {
        3 * DIGEST
    }
    fn num_periodic_columns(&self) -> usize {
        pc::COUNT
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

impl<AB: AirBuilder<F = F>> Air<AB> for TxAir {
    fn eval(&self, builder: &mut AB) {
        let main = builder.main();
        let (lc, nx) = (main.current_slice().to_vec(), main.next_slice().to_vec());
        let pv: Vec<AB::Expr> = builder.public_values().iter().map(|&x| x.into()).collect();
        let per: Vec<AB::Expr> = builder.periodic_values().iter().map(|&x| x.into()).collect();
        let e = |v: AB::Var| -> AB::Expr { v.into() };
        let q = |i: usize| per[i].clone();

        {
            let mut t = builder.when_transition();
            // Poseidon2 rounds.
            let mut full: [AB::Expr; SW] = std::array::from_fn(|i| {
                let x = e(lc[S + i]) + q(pc::RC + i);
                x.clone() * x.clone() * x
            });
            LL::external_linear_layer(&mut full);
            let mut part: [AB::Expr; SW] = std::array::from_fn(|i| {
                if i == 0 {
                    let x = e(lc[S]) + q(pc::RC);
                    x.clone() * x.clone() * x
                } else {
                    e(lc[S + i])
                }
            });
            LL::internal_linear_layer(&mut part);
            let in_round = q(pc::IS_FULL) + q(pc::IS_PART);
            for i in 0..SW {
                t.assert_zero(
                    in_round.clone() * e(nx[S + i]) - q(pc::IS_FULL) * full[i].clone() - q(pc::IS_PART) * part[i].clone(),
                );
            }
            // Permutation boundary: next row 0 = M_E(input).
            let (sc, sco, sm, sf) = (q(pc::SEL_START), q(pc::SEL_CONT), q(pc::SEL_MERKLE), q(pc::SEL_FROM_PREV));
            let b = e(nx[BIT]);
            let iv = |i: usize| match i {
                0 => q(pc::IV_DOM),
                1 => AB::Expr::from_u32(CHAIN_ID),
                2 => q(pc::IV_LEN),
                _ => AB::Expr::ZERO,
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
                    (sc.clone() + sf.clone()) * iv(i) + sco.clone() * e(lc[S + j]) + sm.clone() * (sib.clone() + b.clone() * (cur - sib))
                }
            });
            LL::external_linear_layer(&mut input);
            let any = sc + sco + sm + sf;
            for j in 0..SW {
                t.assert_zero(any.clone() * e(nx[S + j]) - input[j].clone());
            }
            // Position limbs.
            t.assert_zero(e(nx[ACC_LO]) - e(lc[ACC_LO]) - q(pc::W_LO_NEXT) * e(nx[BIT]));
            t.assert_zero(e(nx[ACC_HI]) - e(lc[ACC_HI]) - q(pc::W_HI_NEXT) * e(nx[BIT]));
            // Carried chunk (nsk into the ak permutation; y inside HM permutations).
            for i in 0..RATE {
                t.assert_zero(q(pc::CARRY_NEXT) * (e(nx[CHUNK + i]) - e(lc[CHUNK + i])));
            }
            // Registers held within a note.
            for c in REGS {
                t.assert_zero(q(pc::KEEP_REGS) * (e(nx[c]) - e(lc[c])));
            }
            // A·y: AY_i(next) = (1 − reset)·AY_i + A[i][j]·y_j, y_j = Σ_t YT_t · next.chunk_t.
            let y_next = (0..RATE).fold(AB::Expr::ZERO, |acc, t_| acc + q(pc::YT_NEXT + t_) * e(nx[CHUNK + t_]));
            for i in 0..HM_T {
                t.assert_zero(
                    e(nx[AY + i]) - (AB::Expr::ONE - q(pc::AY_RESET)) * e(lc[AY + i]) - q(pc::COEF_NEXT + i) * y_next.clone(),
                );
            }
        }

        let bit = e(lc[BIT]);
        builder.assert_zero(q(pc::BIT_ROW) * bit.clone() * (bit.clone() - AB::Expr::ONE));
        builder.assert_zero((AB::Expr::ONE - q(pc::BIT_ROW)) * bit);
        builder.when_first_row().assert_zero(e(lc[ACC_LO]));
        builder.when_first_row().assert_zero(e(lc[ACC_HI]));

        for i in 0..RATE {
            let ch = e(lc[CHUNK + i]);
            builder.assert_zero(q(pc::EQ_NK) * (ch.clone() - e(lc[REG_NK + i])));
            builder.assert_zero(q(pc::EQ_PSI) * (ch.clone() - e(lc[REG_PSI + i])));
            builder.assert_zero(q(pc::ZERO) * ch.clone());
            if i >= 3 {
                builder.assert_zero(q(pc::PAD3) * ch.clone());
            }
            if i >= 4 {
                builder.assert_zero(q(pc::PAD4) * ch.clone());
            }
            if i >= 2 {
                builder.assert_zero(q(pc::POS) * ch.clone());
            }
            // b blocks: chunk = x − A·y, with x = owner ‖ limbs(value) ‖ ψ from the registers.
            for blk in 0..3 {
                let el = 12 * blk + i;
                let want = if el < HM_T {
                    let x = if el < 12 {
                        e(lc[REG_OWNER + el])
                    } else if el < 16 {
                        e(lc[REG_VAL + el - 12])
                    } else {
                        e(lc[REG_PSI + el - 16])
                    };
                    x - e(lc[AY + el])
                } else {
                    AB::Expr::ZERO
                };
                builder.assert_zero(q(pc::B + blk) * (ch.clone() - want));
            }
        }
        builder.assert_zero(q(pc::POS) * (e(lc[CHUNK]) - e(lc[ACC_LO])));
        builder.assert_zero(q(pc::POS) * (e(lc[CHUNK + 1]) - e(lc[ACC_HI])));
        for i in 0..DIGEST {
            let out = e(lc[S + i]);
            builder.assert_zero(q(pc::LOAD_NK) * (e(lc[REG_NK + i]) - out.clone()));
            builder.assert_zero(q(pc::LOAD_OWNER) * (e(lc[REG_OWNER + i]) - out.clone()));
            builder.assert_zero(q(pc::ROOT) * (out.clone() - pv[i].clone()));
            builder.assert_zero(q(pc::NF) * (out.clone() - pv[DIGEST + i].clone()));
            builder.assert_zero(q(pc::CM_OUT) * (out - pv[2 * DIGEST + i].clone()));
        }
    }
}

impl crate::stark::ProvableAir for TxAir {
    fn trace_height(&self) -> usize {
        self.height
    }
    fn air_id(&self) -> &'static str {
        "mazze/shielded/m2-in-out/v1"
    }
}

/// Witness: one input (with its spend key) and one output.
#[derive(Clone)]
pub struct TxWitness {
    pub nsk: Digest,
    pub input: Note,
    pub path: [Digest; DEPTH],
    pub pos: u64,
    pub output: Note,
}

pub(crate) fn perm_rows(input: [F; SW]) -> [[F; SW]; R] {
    let mut rows = [[F::ZERO; SW]; R];
    let mut st = input;
    LL::external_linear_layer(&mut st);
    rows[0] = st;
    for r in 0..R - 1 {
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

pub(crate) fn x_of(owner: &Digest, value: u64, psi: &Digest) -> [F; HM_T] {
    let mut x = [F::ZERO; HM_T];
    x[..12].copy_from_slice(owner);
    x[12..16].copy_from_slice(&limbs(value));
    x[16..].copy_from_slice(psi);
    x
}

pub fn build_trace(w: &TxWitness, height: usize) -> (RowMajorMatrix<F>, Vec<F>) {
    let sched = schedule();
    let n = sched.len();
    let keys = SpendKeys::from_nsk(w.nsk);
    let owner_in = keys.owner(&w.input.d);
    let a = hm_matrix();
    let ay = |y: &[F; HM_L]| -> [F; HM_T] {
        std::array::from_fn(|i| (0..HM_L).fold(F::ZERO, |acc, j| acc + a[i][j] * y[j]))
    };
    let (x_in, x_out) = (x_of(&owner_in, w.input.value, &w.input.psi), x_of(&w.output.owner, w.output.value, &w.output.psi));
    let (ay_in, ay_out) = (ay(&w.input.y), ay(&w.output.y));

    // Row-0 chunks and Merkle bits per permutation.
    let mut chunks = vec![[F::ZERO; RATE]; n];
    let mut bits = vec![F::ZERO; n];
    chunks[idx::NK] = w.nsk;
    chunks[idx::AK] = w.nsk;
    chunks[idx::OWN + 1] = keys.nk;
    chunks[idx::OWN + 2][..3].copy_from_slice(&w.input.d);
    let fill_note = |chunks: &mut Vec<[F; RATE]>, hm0: usize, leaf0: usize, y: &[F; HM_L], x: &[F; HM_T], ayv: &[F; HM_T]| {
        for m in 0..6 {
            let len = (HM_L - 12 * m).min(12);
            chunks[hm0 + m][..len].copy_from_slice(&y[12 * m..12 * m + len]);
        }
        for blk in 0..3 {
            for i in 0..RATE {
                let el = 12 * blk + i;
                if el < HM_T {
                    chunks[leaf0 + 1 + blk][i] = x[el] - ayv[el];
                }
            }
        }
    };
    fill_note(&mut chunks, idx::HM_IN, idx::LEAF_IN, &w.input.y, &x_in, &ay_in);
    fill_note(&mut chunks, idx::HM_OUT, idx::LEAF_OUT, &w.output.y, &x_out, &ay_out);
    for k in 0..DEPTH {
        chunks[idx::MERKLE + k] = w.path[k];
        bits[idx::MERKLE + k] = F::new(((w.pos >> k) & 1) as u32);
    }
    chunks[idx::MK] = keys.nk;
    chunks[idx::MK + 1] = w.input.psi;
    chunks[idx::NF + 1][0] = F::new((w.pos & 0xffff) as u32);
    chunks[idx::NF + 1][1] = F::new(((w.pos >> 16) & 0xffff) as u32);

    // Native chaining (mirrors the boundary constraints).
    let iv = |d: u32, len: u32| {
        let mut c = [F::ZERO; RATE];
        c[0] = F::new(d);
        c[1] = F::new(CHAIN_ID);
        c[2] = F::new(len);
        c
    };
    let nperm = height / R;
    let mut inputs = vec![[F::ZERO; SW]; nperm];
    let mut outs = vec![[F::ZERO; SW]; nperm];
    let mut zero = [F::ZERO; SW];
    p2_permute(&mut zero);
    outs[0] = zero;
    for k in 1..n {
        let (prev, ch) = (outs[k - 1], chunks[k]);
        let mut input = [F::ZERO; SW];
        match sched[k].unwrap().kind {
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
        inputs[k] = input;
        let mut o = input;
        p2_permute(&mut o);
        outs[k] = o;
    }
    for k in n..nperm {
        outs[k] = zero;
    }

    let mut values = Vec::with_capacity(height * COLS);
    let (mut acc_lo, mut acc_hi) = (F::ZERO, F::ZERO);
    let mut ayacc = [F::ZERO; HM_T];
    let mut merkle_level = 0usize;
    for (k, input) in inputs.iter().enumerate() {
        let rows = perm_rows(*input);
        assert_eq!(rows[31].map(|x| x.as_canonical_u32()), outs[k].map(|x| x.as_canonical_u32()), "perm {k}");
        let perm = if k < n { sched[k] } else { None };
        // Padding after the output keeps the output's registers (KEEP_REGS runs to the end).
        let in_output_note = k >= idx::HM_OUT;
        let (owner, val, psi, nk) = if in_output_note {
            (w.output.owner, w.output.value, w.output.psi, [F::ZERO; DIGEST])
        } else {
            (owner_in, w.input.value, w.input.psi, keys.nk)
        };
        if let Some(p) = perm {
            if p.kind == Kind::Merkle {
                let wgt = F::new(1 << (merkle_level % 16));
                if merkle_level < 16 { acc_lo += wgt * bits[k] } else { acc_hi += wgt * bits[k] }
                merkle_level += 1;
            }
        }
        for (r, st) in rows.iter().enumerate() {
            let carry = perm.map(|p| p.carry).unwrap_or(false);
            let ch = if k < n && (r == 0 || carry) { chunks[k] } else { [F::ZERO; RATE] };
            let bit = if k < n && r == 0 { bits[k] } else { F::ZERO };
            if let Some(m) = perm.and_then(|p| p.hm) {
                if m == 0 && r == 0 {
                    ayacc = [F::ZERO; HM_T];
                }
                let len = (HM_L - 12 * m).min(12);
                if r < len {
                    let j = 12 * m + r;
                    let y = if in_output_note { w.output.y[j] } else { w.input.y[j] };
                    for i in 0..HM_T {
                        ayacc[i] += a[i][j] * y;
                    }
                }
            }
            values.extend_from_slice(st);
            values.extend_from_slice(&ch);
            values.push(bit);
            values.push(acc_lo);
            values.push(acc_hi);
            values.extend_from_slice(&nk);
            values.extend_from_slice(&owner);
            values.extend_from_slice(&psi);
            values.extend_from_slice(&limbs(val));
            values.extend_from_slice(&ayacc);
        }
    }
    let mut pv = outs[idx::MERKLE + DEPTH - 1][..DIGEST].to_vec();
    pv.extend_from_slice(&outs[idx::NF + 1][..DIGEST]);
    pv.extend_from_slice(&outs[idx::COUNT - 1][..DIGEST]);
    (RowMajorMatrix::new(values, COLS), pv)
}
