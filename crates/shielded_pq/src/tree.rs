//! The append-only depth-32 commitment tree (spec §5). Nodes use the 2→1
//! compression, one permutation per level; the depth is fixed, so levels
//! cannot be confused.

use std::sync::OnceLock;

use p3_field::PrimeCharacteristicRing;

use crate::field::{compress, Digest, DIGEST, F};

pub const DEPTH: usize = 32;

/// `zeros()[l]` is the root of an empty subtree of height `l`.
pub fn zeros() -> &'static [Digest; DEPTH + 1] {
    static Z: OnceLock<[Digest; DEPTH + 1]> = OnceLock::new();
    Z.get_or_init(|| {
        let mut z = [[F::ZERO; DIGEST]; DEPTH + 1];
        for l in 0..DEPTH {
            z[l + 1] = compress(&z[l], &z[l]);
        }
        z
    })
}

/// Root from a leaf, its position and its path. The circuit's direction bits
/// are the bits of `pos`, so the path also binds the position used in `nf`.
pub fn root_from_path(leaf: &Digest, pos: u64, path: &[Digest; DEPTH]) -> Digest {
    let mut cur = *leaf;
    for (l, sib) in path.iter().enumerate() {
        cur = if (pos >> l) & 1 == 0 {
            compress(&cur, sib)
        } else {
            compress(sib, &cur)
        };
    }
    cur
}

/// The whole tree in memory: for wallets and tests. The node keeps only a
/// frontier.
#[derive(Default, Clone)]
pub struct Tree {
    pub leaves: Vec<Digest>,
}

impl Tree {
    pub fn append(&mut self, cm: Digest) -> u64 {
        self.leaves.push(cm);
        (self.leaves.len() - 1) as u64
    }

    fn levels(&self) -> Vec<Vec<Digest>> {
        let z = zeros();
        let mut levels = vec![self.leaves.clone()];
        for l in 0..DEPTH {
            let next = levels[l]
                .chunks(2)
                .map(|p| compress(&p[0], p.get(1).unwrap_or(&z[l])))
                .collect();
            levels.push(next);
        }
        levels
    }

    pub fn root(&self) -> Digest {
        self.levels()[DEPTH].first().copied().unwrap_or(zeros()[DEPTH])
    }

    pub fn path(&self, pos: u64) -> [Digest; DEPTH] {
        let z = zeros();
        let levels = self.levels();
        std::array::from_fn(|l| {
            let sib = ((pos >> l) ^ 1) as usize;
            levels[l].get(sib).copied().unwrap_or(z[l])
        })
    }
}
