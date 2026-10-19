//! M3 + M4: the complete 2-in × 2-out spend statement (spec §7, C1–C10), in the
//! narrow layout (one Poseidon2 round per row).
//!
//! Regions, in order: input A, input B, output C, output D (then padding).
//! Each input runs the M2 chain (nsk → nk/ak → owner → HM opening → leaf →
//! Merkle → mk → nf); each output runs HM opening → leaf.
//!
//! New in M3/M4:
//! - **Ranges (C8).** Every 16-bit limb (4 per note, plus the 12 public
//!   limbs of v_in, v_out and fee) is bit-decomposed over 16 rows: `RBIT` is
//!   boolean, and `RACC` accumulates MSB-first. A check row pins `RACC` to the
//!   limb. Carries are range-checked the same way (3 bits, offset +4).
//! - **Balance (C9).** `BAL_t` accumulates `+value_t` for inputs and `−value_t`
//!   for outputs (one add row per region). A final row checks, limb by limb,
//!   `BAL_t + vin_t − vout_t − fee_t + c_{t−1} = 2^16·c_t` with `c_3 = 0`.
//!   Every term is bounded far below p/2 (limbs < 2^16, carries in [−4, 3]), so
//!   the field equation implies the integer equation: **no wraparound**.
//! - **Dummy gate (C4).** Membership is enforced only when the value is
//!   nonzero. `Z` is computed from the value by an inverse gadget:
//!   `s·INV = Z`, `(1 − Z)·s = 0`, Z boolean, with `s = Σ limbs`. It is never
//!   a free bit the prover chooses.
//! - **Sighash (C10)** is a public input (16 limbs). It isn't constrained by
//!   the AIR; it is bound because public values are absorbed by Fiat–Shamir.

use std::borrow::Cow;

use p3_air::{Air, AirBuilder, BaseAir, WindowAccess};
use p3_field::PrimeCharacteristicRing;
use p3_koala_bear::{
    GenericPoseidon2LinearLayersKoalaBear as LL,
    KOALABEAR_POSEIDON2_RC_24_EXTERNAL_FINAL as RC_END,
    KOALABEAR_POSEIDON2_RC_24_EXTERNAL_INITIAL as RC_BEGIN,
    KOALABEAR_POSEIDON2_RC_24_INTERNAL as RC_PART,
};
use p3_poseidon2::GenericPoseidon2LinearLayers;

use super::perm::{is_full, Chunk, Kind, Perm, R};
use crate::{
    field::{dom, DIGEST, F, RATE, WIDTH as SW},
    note::{hm_matrix, HM_L, HM_T},
    tree::DEPTH,
};

/// Circuit identifier for Fiat–Shamir domain separation.
pub const AIR_ID: &str = "mazze/shielded/spend-2x2/v1";

/// Trace height of the statement (rows).
pub const HEIGHT: usize = 4096;

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
pub const REG_Z: usize = REG_VAL + 4;
pub const REG_INV: usize = REG_Z + 1;
pub const AY: usize = REG_INV + 1;
pub const RBIT: usize = AY + HM_T;
pub const RACC: usize = RBIT + 1;
pub const BAL: usize = RACC + 1;
pub const CARRY: usize = BAL + 4;
pub const COLS: usize = CARRY + 3;
const REGS: std::ops::Range<usize> = REG_NK..AY;

// ---- public values ----
pub mod pvi {
    pub const ANCHOR: usize = 0;
    pub const NF: [usize; 2] = [12, 24];
    pub const CM: [usize; 2] = [36, 48];
    pub const VIN: usize = 60;
    pub const VOUT: usize = 64;
    pub const FEE: usize = 68;
    pub const SIGHASH: usize = 72;
    pub const COUNT: usize = 88;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InLayout {
    pub nk: usize,
    pub ak: usize,
    pub own: usize,
    pub hm: usize,
    pub leaf: usize,
    pub merkle: usize,
    pub mk: usize,
    pub nf: usize,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct OutLayout {
    pub hm: usize,
    pub leaf: usize,
}
#[derive(Clone, Debug)]
pub struct Layout {
    pub sched: Vec<Option<Perm>>,
    pub ins: [InLayout; 2],
    pub outs: [OutLayout; 2],
    /// First permutation of each region A, B, C, D.
    pub region: [usize; 4],
}

pub fn layout() -> Layout {
    use Chunk::*;
    use Kind::*;
    let p = |kind, chunk| Some(Perm { kind, chunk, carry: false, hm: None });
    let mut v: Vec<Option<Perm>> = vec![None];
    let hm_leaf = |v: &mut Vec<Option<Perm>>| -> (usize, usize) {
        let hm = v.len();
        for m in 0..6 {
            let kind = if m == 0 { StartChunk { dom: dom::HM_DIGEST, len: HM_L as u32 } } else { Cont };
            let chunk = if m == 5 { PadAfter(HM_L - 60) } else { Free };
            v.push(Some(Perm { kind, chunk, carry: true, hm: Some(m) }));
        }
        let leaf = v.len();
        v.push(p(StartFromPrev { dom: dom::LEAF, len: 40 }, Zero));
        for k in 0..3 {
            v.push(p(Cont, B(k)));
        }
        (hm, leaf)
    };
    let mut ins = [InLayout::default(); 2];
    for inl in ins.iter_mut() {
        inl.nk = v.len();
        v.push(Some(Perm { kind: StartChunk { dom: dom::NK, len: 12 }, chunk: Free, carry: true, hm: None }));
        inl.ak = v.len();
        v.push(p(StartChunk { dom: dom::AK, len: 12 }, FromPrev));
        inl.own = v.len();
        v.push(p(StartFromPrev { dom: dom::OWN, len: 27 }, Zero));
        v.push(p(Cont, EqNk));
        v.push(p(Cont, PadAfter(3)));
        let (hm, leaf) = hm_leaf(&mut v);
        inl.hm = hm;
        inl.leaf = leaf;
        inl.merkle = v.len();
        for _ in 0..DEPTH {
            v.push(p(Merkle, Free));
        }
        inl.mk = v.len();
        v.push(p(StartChunk { dom: dom::MK, len: 24 }, EqNk));
        v.push(p(Cont, EqPsi));
        inl.nf = v.len();
        v.push(p(StartFromPrev { dom: dom::NF, len: (DIGEST + 2) as u32 }, Zero));
        v.push(p(Cont, Pos));
    }
    let mut outs = [OutLayout::default(); 2];
    for o in outs.iter_mut() {
        let (hm, leaf) = hm_leaf(&mut v);
        *o = OutLayout { hm, leaf };
    }
    let region = [ins[0].nk, ins[1].nk, outs[0].hm, outs[1].hm];
    Layout { sched: v, ins, outs, region }
}

/// A range block: `bits` rows starting at `start`, pinned to `target`.
#[derive(Clone, Copy, Debug)]
pub enum Target {
    Val(usize),
    Pub(usize),
    Carry(usize),
}
pub fn range_blocks(l: &Layout) -> Vec<(usize, usize, Target)> {
    let mut b = vec![];
    for reg in 0..4 {
        let base = l.region[reg] * R + 8;
        for t in 0..4 {
            b.push((base + 16 * t, 16, Target::Val(t)));
        }
    }
    let base = l.region[0] * R + 8 + 64;
    for k in 0..12 {
        b.push((base + 16 * k, 16, Target::Pub(k)));
    }
    let base = base + 16 * 12;
    for t in 0..3 {
        b.push((base + 3 * t, 3, Target::Carry(t)));
    }
    b
}
/// Row (transition) where each region adds its value to BAL, and the check row.
pub fn bal_rows(l: &Layout) -> ([usize; 4], usize) {
    let adds = std::array::from_fn(|reg| l.region[reg] * R + 300);
    (adds, l.region[3] * R + 310)
}

mod pc {
    use super::{HM_T, RATE, SW};
    pub const IS_FULL: usize = 0;
    pub const IS_PART: usize = 1;
    pub const RC: usize = 2;
    pub const ROUND_END: usize = RC + SW;
    pub const SEL_START: usize = ROUND_END;
    pub const SEL_CONT: usize = SEL_START + 1;
    pub const SEL_MERKLE: usize = SEL_START + 2;
    pub const SEL_FROM_PREV: usize = SEL_START + 3;
    pub const IV_DOM: usize = SEL_START + 4;
    pub const IV_LEN: usize = SEL_START + 5;
    pub const BIT_ROW: usize = SEL_START + 6;
    pub const W_LO_NEXT: usize = SEL_START + 7;
    pub const W_HI_NEXT: usize = SEL_START + 8;
    pub const ACC_RESET: usize = SEL_START + 9;
    pub const CARRY_NEXT: usize = SEL_START + 10;
    pub const EQ_NK: usize = SEL_START + 11;
    pub const EQ_PSI: usize = SEL_START + 12;
    pub const PAD3: usize = SEL_START + 13;
    pub const PAD4: usize = SEL_START + 14;
    pub const ZERO: usize = SEL_START + 15;
    pub const POS: usize = SEL_START + 16;
    pub const B: usize = SEL_START + 17; // 3
    pub const LOAD_NK: usize = B + 3;
    pub const LOAD_OWNER: usize = LOAD_NK + 1;
    pub const KEEP_REGS: usize = LOAD_NK + 2;
    pub const AY_RESET: usize = LOAD_NK + 3;
    pub const COEF_NEXT: usize = LOAD_NK + 4; // HM_T
    pub const YT_NEXT: usize = COEF_NEXT + HM_T; // RATE
    pub const ROOT: usize = YT_NEXT + RATE;
    pub const NF_A: usize = ROOT + 1;
    pub const NF_B: usize = ROOT + 2;
    pub const CM_C: usize = ROOT + 3;
    pub const CM_D: usize = ROOT + 4;
    pub const IN_REGION: usize = ROOT + 5;
    pub const R_START: usize = ROOT + 6;
    pub const R_STEP: usize = ROOT + 7;
    pub const CHECK_VAL: usize = ROOT + 8; // 4
    pub const CHECK_PUB: usize = CHECK_VAL + 4; // 12
    pub const CHECK_CARRY: usize = CHECK_PUB + 12; // 3
    pub const BAL_ADD: usize = CHECK_CARRY + 3;
    pub const BAL_CHECK: usize = BAL_ADD + 1;
    pub const COUNT: usize = BAL_CHECK + 1;
}

pub struct Tx2Air {
    pub height: usize,
    /// Mixed into the capacity of every sponge in the statement.
    pub chain_id: u32,
    periodic: Vec<Vec<F>>,
}

impl Tx2Air {
    pub fn new(chain_id: u32) -> Self { Self::with_height(chain_id, HEIGHT) }

    pub fn with_height(chain_id: u32, height: usize) -> Self {
        let l = layout();
        let n = l.sched.len();
        assert!(height.is_power_of_two() && height >= n * R);
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
        for row in 0..height - 1 {
            c[pc::KEEP_REGS][row] = F::ONE;
        }
        for reg in 1..4 {
            c[pc::KEEP_REGS][l.region[reg] * R - 1] = F::ZERO;
        }
        for row in (l.region[0] * R)..(l.region[2] * R) {
            c[pc::IN_REGION][row] = F::ONE;
        }
        // Merkle levels count from 0 within each input; the position resets per input.
        for inl in &l.ins {
            c[pc::ACC_RESET][inl.merkle * R - 1] = F::ONE;
        }
        for (k, perm) in l.sched.iter().enumerate().skip(1) {
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
                    let first = l.ins.iter().find(|i| (i.merkle..i.merkle + DEPTH).contains(&k)).unwrap().merkle;
                    let level = k - first;
                    let w = F::new(1 << (level % 16));
                    c[if level < 16 { pc::W_LO_NEXT } else { pc::W_HI_NEXT }][prev_end] = w;
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
                let len = (HM_L - 12 * m).min(12);
                for r in 0..len {
                    let j = 12 * m + r;
                    for i in 0..HM_T {
                        c[pc::COEF_NEXT + i][row0 + r - 1] = a[i][j];
                    }
                    c[pc::YT_NEXT + r][row0 + r - 1] = F::ONE;
                }
            }
        }
        for (x, inl) in l.ins.iter().enumerate() {
            c[pc::LOAD_NK][(inl.nk + 1) * R - 1] = F::ONE;
            c[pc::LOAD_OWNER][(inl.own + 3) * R - 1] = F::ONE;
            c[pc::ROOT][(inl.merkle + DEPTH) * R - 1] = F::ONE;
            c[if x == 0 { pc::NF_A } else { pc::NF_B }][(inl.nf + 2) * R - 1] = F::ONE;
        }
        c[pc::CM_C][(l.outs[0].leaf + 4) * R - 1] = F::ONE;
        c[pc::CM_D][(l.outs[1].leaf + 4) * R - 1] = F::ONE;
        for (start, bits, target) in range_blocks(&l) {
            c[pc::R_START][start] = F::ONE;
            for i in 0..bits - 1 {
                c[pc::R_STEP][start + i] = F::ONE;
            }
            let col = match target {
                Target::Val(t) => pc::CHECK_VAL + t,
                Target::Pub(k) => pc::CHECK_PUB + k,
                Target::Carry(t) => pc::CHECK_CARRY + t,
            };
            c[col][start + bits - 1] = F::ONE;
        }
        let (adds, check) = bal_rows(&l);
        for (reg, row) in adds.iter().enumerate() {
            c[pc::BAL_ADD][*row] = if reg < 2 { F::ONE } else { F::ZERO - F::ONE };
        }
        c[pc::BAL_CHECK][check] = F::ONE;
        Tx2Air {
            height,
            chain_id,
            periodic: c,
        }
    }
}

impl BaseAir<F> for Tx2Air {
    fn width(&self) -> usize {
        COLS
    }
    fn num_public_values(&self) -> usize {
        pvi::COUNT
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

impl<AB: AirBuilder<F = F>> Air<AB> for Tx2Air {
    fn eval(&self, builder: &mut AB) {
        let main = builder.main();
        let (lc, nx) = (main.current_slice().to_vec(), main.next_slice().to_vec());
        let pv: Vec<AB::Expr> = builder.public_values().iter().map(|&x| x.into()).collect();
        let per: Vec<AB::Expr> = builder.periodic_values().iter().map(|&x| x.into()).collect();
        let e = |v: AB::Var| -> AB::Expr { v.into() };
        let q = |i: usize| per[i].clone();
        let two16 = AB::Expr::from_u32(1 << 16);

        {
            let mut t = builder.when_transition();
            // Poseidon2 rounds (C1–C7 hashes).
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
            // Permutation boundary.
            let (sc, sco, sm, sf) = (q(pc::SEL_START), q(pc::SEL_CONT), q(pc::SEL_MERKLE), q(pc::SEL_FROM_PREV));
            let b = e(nx[BIT]);
            let iv = |i: usize| match i {
                0 => q(pc::IV_DOM),
                1 => AB::Expr::from_u32(self.chain_id),
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
            // Position limbs, reset per input.
            let keep_acc = AB::Expr::ONE - q(pc::ACC_RESET);
            t.assert_zero(e(nx[ACC_LO]) - keep_acc.clone() * e(lc[ACC_LO]) - q(pc::W_LO_NEXT) * e(nx[BIT]));
            t.assert_zero(e(nx[ACC_HI]) - keep_acc * e(lc[ACC_HI]) - q(pc::W_HI_NEXT) * e(nx[BIT]));
            // Carried chunk.
            for i in 0..RATE {
                t.assert_zero(q(pc::CARRY_NEXT) * (e(nx[CHUNK + i]) - e(lc[CHUNK + i])));
            }
            // Per-note registers.
            for c in REGS {
                t.assert_zero(q(pc::KEEP_REGS) * (e(nx[c]) - e(lc[c])));
            }
            // A·y accumulation.
            let y_next = (0..RATE).fold(AB::Expr::ZERO, |acc, k| acc + q(pc::YT_NEXT + k) * e(nx[CHUNK + k]));
            for i in 0..HM_T {
                t.assert_zero(
                    e(nx[AY + i]) - (AB::Expr::ONE - q(pc::AY_RESET)) * e(lc[AY + i]) - q(pc::COEF_NEXT + i) * y_next.clone(),
                );
            }
            // Range accumulation (MSB first).
            t.assert_zero(q(pc::R_STEP) * (e(nx[RACC]) - AB::Expr::TWO * e(lc[RACC]) - e(nx[RBIT])));
            // Balance accumulators and global carries.
            for k in 0..4 {
                t.assert_zero(e(nx[BAL + k]) - e(lc[BAL + k]) - q(pc::BAL_ADD) * e(lc[REG_VAL + k]));
            }
            for k in 0..3 {
                t.assert_zero(e(nx[CARRY + k]) - e(lc[CARRY + k]));
            }
        }

        // Merkle bits.
        let bit = e(lc[BIT]);
        builder.assert_zero(q(pc::BIT_ROW) * bit.clone() * (bit.clone() - AB::Expr::ONE));
        builder.assert_zero((AB::Expr::ONE - q(pc::BIT_ROW)) * bit);
        builder.when_first_row().assert_zero(e(lc[ACC_LO]));
        builder.when_first_row().assert_zero(e(lc[ACC_HI]));
        for k in 0..4 {
            builder.when_first_row().assert_zero(e(lc[BAL + k]));
        }

        // Row-0 chunk rules and the HM b-check.
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

        // Dummy gate: Z = [value ≠ 0], computed (not chosen) on input regions.
        let s = (0..4).fold(AB::Expr::ZERO, |acc, k| acc + e(lc[REG_VAL + k]));
        let z = e(lc[REG_Z]);
        builder.assert_zero(q(pc::IN_REGION) * (s.clone() * e(lc[REG_INV]) - z.clone()));
        builder.assert_zero(q(pc::IN_REGION) * (AB::Expr::ONE - z.clone()) * s);
        builder.assert_zero(q(pc::IN_REGION) * z.clone() * (z.clone() - AB::Expr::ONE));

        // Register loads and public outputs.
        for i in 0..DIGEST {
            let out = e(lc[S + i]);
            builder.assert_zero(q(pc::LOAD_NK) * (e(lc[REG_NK + i]) - out.clone()));
            builder.assert_zero(q(pc::LOAD_OWNER) * (e(lc[REG_OWNER + i]) - out.clone()));
            // C4: membership only for real (nonzero) inputs.
            builder.assert_zero(q(pc::ROOT) * z.clone() * (out.clone() - pv[pvi::ANCHOR + i].clone()));
            builder.assert_zero(q(pc::NF_A) * (out.clone() - pv[pvi::NF[0] + i].clone()));
            builder.assert_zero(q(pc::NF_B) * (out.clone() - pv[pvi::NF[1] + i].clone()));
            builder.assert_zero(q(pc::CM_C) * (out.clone() - pv[pvi::CM[0] + i].clone()));
            builder.assert_zero(q(pc::CM_D) * (out - pv[pvi::CM[1] + i].clone()));
        }

        // C8: ranges.
        let rb = e(lc[RBIT]);
        builder.assert_zero(rb.clone() * (rb.clone() - AB::Expr::ONE));
        builder.assert_zero(q(pc::R_START) * (e(lc[RACC]) - rb));
        let racc = e(lc[RACC]);
        for k in 0..4 {
            builder.assert_zero(q(pc::CHECK_VAL + k) * (racc.clone() - e(lc[REG_VAL + k])));
        }
        for k in 0..12 {
            builder.assert_zero(q(pc::CHECK_PUB + k) * (racc.clone() - pv[pvi::VIN + k].clone()));
        }
        for k in 0..3 {
            builder.assert_zero(q(pc::CHECK_CARRY + k) * (racc.clone() - e(lc[CARRY + k]) - AB::Expr::from_u32(4)));
        }

        // C9: limb-wise balance with carries; c_{-1} = c_3 = 0.
        for k in 0..4 {
            let carry_in = if k == 0 { AB::Expr::ZERO } else { e(lc[CARRY + k - 1]) };
            let carry_out = if k == 3 { AB::Expr::ZERO } else { e(lc[CARRY + k]) };
            builder.assert_zero(
                q(pc::BAL_CHECK)
                    * (e(lc[BAL + k]) + pv[pvi::VIN + k].clone() - pv[pvi::VOUT + k].clone() - pv[pvi::FEE + k].clone() + carry_in
                        - two16.clone() * carry_out),
            );
        }
    }
}

impl crate::ps1::Ps1Air for Tx2Air {
    fn air_id(&self) -> &'static str { AIR_ID }
}

#[cfg(feature = "prover")]
mod prover;
#[cfg(feature = "prover")]
pub use prover::*;
