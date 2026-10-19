//! Append-only depth-32 commitment tree (spec §5). Nodes use the 2→1
//! compression (one permutation per level). The depth is fixed, so levels
//! can't be confused.
//! The spike recomputes levels on demand; the node keeps a frontier instead.

use std::sync::OnceLock;

use p3_field::PrimeCharacteristicRing;

use crate::primitives::*;

pub const DEPTH: usize = 32;

pub fn node(_level: usize, l: &Digest, r: &Digest) -> Digest {
    compress(l, r)
}

fn zeros() -> &'static [Digest; DEPTH + 1] {
    static Z: OnceLock<[Digest; DEPTH + 1]> = OnceLock::new();
    Z.get_or_init(|| {
        let mut z = [[F::ZERO; DIGEST]; DEPTH + 1];
        for l in 0..DEPTH {
            z[l + 1] = node(l, &z[l], &z[l]);
        }
        z
    })
}

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
            let cur = &levels[l];
            let next = cur
                .chunks(2)
                .map(|p| node(l, &p[0], p.get(1).unwrap_or(&z[l])))
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

    pub fn find(&self, cm: &Digest) -> Option<u64> {
        self.leaves.iter().position(|x| x == cm).map(|p| p as u64)
    }
}

/// Root from a leaf, its position and its path. The circuit's direction bits
/// are the bits of `pos`, so the path also binds the position used in `nf`.
pub fn root_from_path(leaf: &Digest, pos: u64, path: &[Digest; DEPTH]) -> Digest {
    let mut cur = *leaf;
    for (l, sib) in path.iter().enumerate() {
        cur = if (pos >> l) & 1 == 0 { node(l, &cur, sib) } else { node(l, sib, &cur) };
    }
    cur
}
