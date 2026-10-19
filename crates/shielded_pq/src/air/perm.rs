//! The narrow Poseidon2 layout shared by the AIRs: one round per row, 32 rows
//! per permutation (4 full rounds, 23 partial, 4 full, one output row).

use p3_field::PrimeCharacteristicRing;
use p3_koala_bear::{
    GenericPoseidon2LinearLayersKoalaBear as LL,
    KOALABEAR_POSEIDON2_RC_24_EXTERNAL_FINAL as RC_END,
    KOALABEAR_POSEIDON2_RC_24_EXTERNAL_INITIAL as RC_BEGIN,
    KOALABEAR_POSEIDON2_RC_24_INTERNAL as RC_PART,
};
use p3_poseidon2::GenericPoseidon2LinearLayers;

use crate::field::{F, WIDTH as SW};

/// Rows per permutation.
pub const R: usize = 32;

pub const fn is_full(r: usize) -> bool { r < 4 || (r >= 27 && r < 31) }

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

/// The 32 rows of one permutation (row 0 is the input after the initial
/// linear layer; row 31 is the output).
pub fn perm_rows(input: [F; SW]) -> [[F; SW]; R] {
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
