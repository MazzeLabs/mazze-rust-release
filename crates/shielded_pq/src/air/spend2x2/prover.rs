//! Witness and trace generation for the 2×2 statement (wallet side).

use p3_field::{Field, PrimeCharacteristicRing, PrimeField32};
use p3_matrix::dense::RowMajorMatrix;

use super::*;
use crate::{
    air::perm::perm_rows,
    field::{limbs, p2_permute, Digest, Sponge},
    keys::SpendKeys,
    note::{message, Note},
};

/// One real or dummy input: its spend key, note, path and position.
#[derive(Clone)]
pub struct SpendIn {
    pub nsk: Digest,
    pub note: Note,
    pub path: [Digest; DEPTH],
    pub pos: u64,
}

#[derive(Clone)]
pub struct Tx2Witness {
    pub inputs: [SpendIn; 2],
    pub outputs: [Note; 2],
    pub v_in: u64,
    pub v_out: u64,
    pub fee: u64,
    pub sighash: [u8; 32],
    /// The anchor the transaction claims (the real tree root).
    pub anchor: Digest,
    /// Tests only: raw field limbs for an output's value (to forge "negative"
    /// limbs that no u64 can express).
    pub out_limb_override: [Option<[i64; 4]>; 2],
}

/// Value limbs of a note as signed integers (honest: 0..2^16).
fn limbs_i(v: u64) -> [i64; 4] {
    std::array::from_fn(|t| ((v >> (16 * t)) & 0xffff) as i64)
}

impl Tx2Witness {
    pub fn out_limbs(&self, x: usize) -> [i64; 4] {
        self.out_limb_override[x].unwrap_or_else(|| limbs_i(self.outputs[x].value))
    }
}

fn to_f(x: i64) -> F {
    if x >= 0 { F::new(x as u32) } else { F::ZERO - F::new((-x) as u32) }
}

/// Carries for the limb-wise balance, or None if the transaction doesn't balance.
pub fn carries(w: &Tx2Witness) -> Option<[i64; 3]> {
    let lim = |v: u64, t: usize| ((v >> (16 * t)) & 0xffff) as i64;
    let (oc, od) = (w.out_limbs(0), w.out_limbs(1));
    let mut c = [0i64; 4];
    let mut carry = 0i64;
    for t in 0..4 {
        let d = lim(w.inputs[0].note.value, t) + lim(w.inputs[1].note.value, t) + lim(w.v_in, t)
            - oc[t]
            - od[t]
            - lim(w.v_out, t)
            - lim(w.fee, t)
            + carry;
        if d.rem_euclid(1 << 16) != 0 {
            return None;
        }
        carry = d.div_euclid(1 << 16);
        c[t] = carry;
    }
    (c[3] == 0).then_some([c[0], c[1], c[2]])
}

pub fn public_values(w: &Tx2Witness, nf: [Digest; 2], cm: [Digest; 2]) -> Vec<F> {
    let mut pv = w.anchor.to_vec();
    pv.extend_from_slice(&nf[0]);
    pv.extend_from_slice(&nf[1]);
    pv.extend_from_slice(&cm[0]);
    pv.extend_from_slice(&cm[1]);
    pv.extend_from_slice(&limbs(w.v_in));
    pv.extend_from_slice(&limbs(w.v_out));
    pv.extend_from_slice(&limbs(w.fee));
    for c in w.sighash.chunks(2) {
        pv.push(F::new(u16::from_le_bytes([c[0], c[1]]) as u32));
    }
    pv
}

/// Build the trace. `carries_override` lets tests forge carries.
pub fn build_trace(
    w: &Tx2Witness, chain_id: u32, height: usize, carries_override: Option<[i64; 3]>,
) -> (RowMajorMatrix<F>, Vec<F>) {
    let l = layout();
    let n = l.sched.len();
    let a = hm_matrix();
    let sponge = Sponge::new(chain_id);
    let keys = [SpendKeys::from_nsk(&sponge, w.inputs[0].nsk), SpendKeys::from_nsk(&sponge, w.inputs[1].nsk)];
    let owners_in = [keys[0].owner(&sponge, &w.inputs[0].note.d), keys[1].owner(&sponge, &w.inputs[1].note.d)];
    let ay = |y: &[F; HM_L]| -> [F; HM_T] {
        std::array::from_fn(|i| (0..HM_L).fold(F::ZERO, |acc, j| acc + a[i][j] * y[j]))
    };
    let mut chunks = vec![[F::ZERO; RATE]; n];
    let mut bits = vec![F::ZERO; n];
    let fill_hm_leaf = |chunks: &mut Vec<[F; RATE]>, hm: usize, leaf: usize, note: &Note, owner: &Digest, lim: Option<[i64; 4]>| {
        let mut x = message(owner, note.value, &note.psi);
        if let Some(lv) = lim {
            for t in 0..4 {
                x[12 + t] = to_f(lv[t]);
            }
        }
        let ayv = ay(&note.y);
        for m in 0..6 {
            let len = (HM_L - 12 * m).min(12);
            chunks[hm + m][..len].copy_from_slice(&note.y[12 * m..12 * m + len]);
        }
        for blk in 0..3 {
            for i in 0..RATE {
                let el = 12 * blk + i;
                if el < HM_T {
                    chunks[leaf + 1 + blk][i] = x[el] - ayv[el];
                }
            }
        }
    };
    for (x, inl) in l.ins.iter().enumerate() {
        let sp = &w.inputs[x];
        chunks[inl.nk] = sp.nsk;
        chunks[inl.ak] = sp.nsk;
        chunks[inl.own + 1] = keys[x].nk;
        chunks[inl.own + 2][..3].copy_from_slice(&sp.note.d);
        fill_hm_leaf(&mut chunks, inl.hm, inl.leaf, &sp.note, &owners_in[x], None);
        for k in 0..DEPTH {
            chunks[inl.merkle + k] = sp.path[k];
            bits[inl.merkle + k] = F::new(((sp.pos >> k) & 1) as u32);
        }
        chunks[inl.nf] = keys[x].nk;
        chunks[inl.nf + 1] = sp.note.psi;
        chunks[inl.nf + 2][0] = F::new((sp.pos & 0xffff) as u32);
        chunks[inl.nf + 2][1] = F::new(((sp.pos >> 16) & 0xffff) as u32);
    }
    for (x, o) in l.outs.iter().enumerate() {
        fill_hm_leaf(&mut chunks, o.hm, o.leaf, &w.outputs[x], &w.outputs[x].owner, w.out_limb_override[x]);
    }

    let iv = |d: u32, len: u32| {
        let mut c = [F::ZERO; RATE];
        c[0] = F::new(d);
        c[1] = F::new(chain_id);
        c[2] = F::new(len);
        c
    };
    let nperm = height / R;
    let mut inputs = vec![[F::ZERO; SW]; nperm];
    let mut outs = vec![[F::ZERO; SW]; nperm];
    let mut zero = [F::ZERO; SW];
    p2_permute(&mut zero);
    for k in 0..nperm {
        outs[k] = zero;
    }
    for k in 1..n {
        let (prev, ch) = (outs[k - 1], chunks[k]);
        let mut input = [F::ZERO; SW];
        match l.sched[k].unwrap().kind {
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
                let (lft, rgt) = if bits[k] == F::ZERO { (cur, &ch[..]) } else { (&ch[..], cur) };
                input[..RATE].copy_from_slice(lft);
                input[RATE..].copy_from_slice(rgt);
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
    let nf = [outs[l.ins[0].nf + 2], outs[l.ins[1].nf + 2]].map(|o| <Digest>::try_from(&o[..DIGEST]).unwrap());
    let cm = [outs[l.outs[0].leaf + 3], outs[l.outs[1].leaf + 3]].map(|o| <Digest>::try_from(&o[..DIGEST]).unwrap());
    let pv = public_values(w, nf, cm);

    // Per-region register values.
    struct Regs {
        nk: Digest,
        owner: Digest,
        psi: Digest,
        val: u64,
        lim: [F; 4],
        z: F,
        inv: F,
    }
    let regs_of = |reg: usize| -> Regs {
        if reg < 2 {
            let sp = &w.inputs[reg];
            let s: F = limbs(sp.note.value).iter().copied().sum();
            let (z, inv) = if s == F::ZERO { (F::ZERO, F::ZERO) } else { (F::ONE, s.inverse()) };
            Regs { nk: keys[reg].nk, owner: owners_in[reg], psi: sp.note.psi, val: sp.note.value, lim: limbs(sp.note.value), z, inv }
        } else {
            let o = &w.outputs[reg - 2];
            let lim = w.out_limbs(reg - 2).map(to_f);
            Regs { nk: [F::ZERO; DIGEST], owner: o.owner, psi: o.psi, val: o.value, lim, z: F::ZERO, inv: F::ZERO }
        }
    };
    let region_of_perm = |k: usize| (0..4).rev().find(|&r| k >= l.region[r]).unwrap_or(0);

    // Range bits per row.
    let carry_v = carries_override.or_else(|| carries(w)).unwrap_or([0; 3]);
    let mut rbit = vec![F::ZERO; height];
    let mut racc = vec![F::ZERO; height];
    for (start, nbits, target) in range_blocks(&l) {
        let value: u64 = match target {
            Target::Val(t) => {
                let reg = (0..4).rev().find(|&r| start >= l.region[r] * R).unwrap();
                (regs_of(reg).val >> (16 * t)) & 0xffff
            }
            Target::Pub(k) => [w.v_in, w.v_out, w.fee][k / 4] >> (16 * (k % 4)) & 0xffff,
            Target::Carry(t) => (carry_v[t] + 4).clamp(0, 7) as u64,
        };
        let mut acc = 0u64;
        for i in 0..nbits {
            let b = (value >> (nbits - 1 - i)) & 1;
            acc = 2 * acc + b;
            rbit[start + i] = F::new(b as u32);
            racc[start + i] = F::new(acc as u32);
        }
    }
    let (bal_adds, _) = bal_rows(&l);
    let carry_f = carry_v.map(to_f);

    let mut values = Vec::with_capacity(height * COLS);
    let (mut acc_lo, mut acc_hi) = (F::ZERO, F::ZERO);
    let mut ayacc = [F::ZERO; HM_T];
    let mut bal = [F::ZERO; 4];
    let merkle_first = |k: usize| l.ins.iter().find(|i| (i.merkle..i.merkle + DEPTH).contains(&k)).map(|i| i.merkle);
    for (k, input) in inputs.iter().enumerate() {
        let rows = perm_rows(*input);
        assert_eq!(rows[31].map(|x| x.as_canonical_u32()), outs[k].map(|x| x.as_canonical_u32()), "perm {k}");
        let perm = if k < n { l.sched[k] } else { None };
        let reg = region_of_perm(k);
        let rg = regs_of(reg);
        let note_y = if reg < 2 { w.inputs[reg].note.y } else { w.outputs[reg - 2].y };
        if let Some(first) = merkle_first(k) {
            if k == first {
                acc_lo = F::ZERO;
                acc_hi = F::ZERO;
            }
            let level = k - first;
            let wgt = F::new(1 << (level % 16));
            if level < 16 { acc_lo += wgt * bits[k] } else { acc_hi += wgt * bits[k] }
        }
        for (r, st) in rows.iter().enumerate() {
            let row = k * R + r;
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
                    for i in 0..HM_T {
                        ayacc[i] += a[i][j] * note_y[j];
                    }
                }
            }
            values.extend_from_slice(st);
            values.extend_from_slice(&ch);
            values.push(bit);
            values.push(acc_lo);
            values.push(acc_hi);
            values.extend_from_slice(&rg.nk);
            values.extend_from_slice(&rg.owner);
            values.extend_from_slice(&rg.psi);
            values.extend_from_slice(&rg.lim);
            values.push(rg.z);
            values.push(rg.inv);
            values.extend_from_slice(&ayacc);
            values.push(rbit[row]);
            values.push(racc[row]);
            values.extend_from_slice(&bal);
            values.extend_from_slice(&carry_f);
            // BAL update takes effect on the next row.
            if let Some(reg_add) = bal_adds.iter().position(|&x| x == row) {
                let lv = rg.lim;
                for t in 0..4 {
                    if reg_add < 2 { bal[t] += lv[t] } else { bal[t] -= lv[t] }
                }
            }
        }
    }
    (RowMajorMatrix::new(values, COLS), pv)
}
