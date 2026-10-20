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

/// The tree as the node keeps it: the number of leaves and, per level, the
/// left sibling still waiting for its right half. Appending costs at most
/// one compression per level and the root is recomputed from the frontier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frontier {
    pub leaves: u64,
    /// `filled[l]` is the root of the last complete left subtree of height
    /// `l` whose right sibling has not been appended yet; it is meaningful
    /// only where bit `l` of `leaves` is set.
    pub filled: [Digest; DEPTH],
}

impl Default for Frontier {
    fn default() -> Self {
        Frontier {
            leaves: 0,
            filled: [[F::ZERO; DIGEST]; DEPTH],
        }
    }
}

/// Leaves a depth-32 tree holds.
pub const CAPACITY: u64 = 1 << DEPTH;

impl Frontier {
    /// Appends a leaf and returns its position, or `None` when the tree is
    /// full.
    pub fn append(&mut self, leaf: Digest) -> Option<u64> {
        if self.leaves >= CAPACITY {
            return None;
        }
        let pos = self.leaves;
        let mut cur = leaf;
        for l in 0..DEPTH {
            if (pos >> l) & 1 == 0 {
                self.filled[l] = cur;
                break;
            }
            cur = compress(&self.filled[l], &cur);
        }
        self.leaves += 1;
        Some(pos)
    }

    pub fn root(&self) -> Digest {
        let z = zeros();
        let mut cur = z[0];
        let mut have = false;
        for l in 0..DEPTH {
            if (self.leaves >> l) & 1 == 1 {
                cur = if have {
                    compress(&self.filled[l], &cur)
                } else {
                    compress(&self.filled[l], &z[l])
                };
                have = true;
            } else if have {
                cur = compress(&cur, &z[l]);
            } else {
                cur = z[l + 1];
                continue;
            }
        }
        if have { cur } else { z[DEPTH] }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::Xof;

    fn leaf(i: u64) -> Digest {
        Xof::new("mazze/test/leaf", &[&i.to_le_bytes()]).fields()
    }

    #[test]
    fn frontier_matches_the_full_tree() {
        let mut tree = Tree::default();
        let mut frontier = Frontier::default();
        assert_eq!(frontier.root(), tree.root(), "empty");
        for i in 0..70 {
            let l = leaf(i);
            assert_eq!(frontier.append(l), Some(tree.append(l)));
            assert_eq!(frontier.root(), tree.root(), "after {} leaves", i + 1);
        }
    }

    #[test]
    fn paths_from_the_full_tree_verify_against_the_frontier_root() {
        let mut tree = Tree::default();
        let mut frontier = Frontier::default();
        for i in 0..9 {
            tree.append(leaf(i));
            frontier.append(leaf(i));
        }
        for pos in 0..9 {
            let root = root_from_path(&leaf(pos), pos, &tree.path(pos));
            assert_eq!(root, frontier.root());
        }
    }
}
