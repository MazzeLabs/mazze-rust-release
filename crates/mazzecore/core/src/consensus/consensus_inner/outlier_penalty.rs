// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Outlier-penalty difficulty of an epoch block, computed without scanning the
//! whole arena.
//!
//! The reward of an epoch block `B` (executed in the epoch of main block `M`)
//! is cut when
//!
//! ```text
//!   D(B) = sum weight(x) for x in outlier(B) \ outlier(C), is_same_era(x, M)
//! ```
//!
//! is too large, where `C` is the outlier-penalty cutoff main block
//! (`get_main_reward_index`). `outlier(Y)` is what
//! `ConsensusNewBlockHandler::compute_outlier_bruteforce` returns:
//!
//! ```text
//!   lim(Y)     = max last_main_in_past over Y's parent and referees
//!   PB(Y)      = Y plus the blocks reached from Y over parent/referee edges
//!                through blocks with epoch_number > lim(Y)
//!   F(Y)       = Y plus everything reached over children/referrer edges
//!   outlier(Y) = { x : epoch(x) > lim(Y), x not in PB(Y) or F(Y),
//!                  x preactivated, x.era_block != NULL }
//! ```
//!
//! The brute force walks the whole future of `Y` and scans the whole arena, so
//! one call costs O(arena). An archive node keeps the arena from genesis, and
//! replaying its history (the outlier cache is empty then) made this the whole
//! cost of execution.
//!
//! `outlier_difficulty_windowed` computes the same `D(B)` from a window of
//! about `OUTLIER_PENALTY_UPPER_EPOCH_COUNT` epochs:
//!
//! ```text
//!   D(B) = sum weight(x) for x in W, x not in PB(B), x not in F(B),
//!          x preactivated, x.era_block != NULL, is_same_era(x, M)
//!   W    = C plus the blocks reached from C over parent/referee edges
//!          through blocks with epoch_number > lim(B)
//! ```
//!
//! Proof. It relies on two invariants of the consensus graph, both checked
//! where cheap and otherwise the function falls back to the brute force:
//! (E) epoch_number is the epoch of a block in the current main chain, NULL
//!     for a block outside the past of the main chain; hence `x` in past(y)
//!     implies epoch(x) <= epoch(y), and every block of epoch h is in
//!     past(main[h]) + main[h];
//! (S) children/referrers are exactly the reverse of parent/referees (both are
//!     pushed on insertion; a checkpoint only removes past(new era genesis),
//!     which no kept block has a forward edge into).
//! Guards: C is main block main[epoch(C)], lim(B) < epoch(B) <= epoch(C),
//! lim(C) < epoch(C), and lim(B), lim(C), epoch(B) are in the current era.
//!
//! 1. B in past(C) + C (epoch(B) <= epoch(C) and (E); epoch(B) = epoch(C)
//!    when the era bound makes C = M), so F(C) is a subset of F(B) (S). For
//!    x in outlier(B), x is never in F(C), so
//!    x in outlier(C) <=> epoch(x) > lim(C) and x not in PB(C).
//! 2. If x is in outlier(B) and not in past(C) + C, then x is in outlier(C):
//!    main[lim(C)] is a main ancestor of C, so a block with
//!    epoch <= lim(C) is in past(C) (E); thus epoch(x) > lim(C), and
//!    PB(C) is inside past(C) + C.
//! 3. If x is in past(C) + C, then x is not in outlier(C): either
//!    epoch(x) <= lim(C), or every block on a parent/referee path from C to x
//!    has x in its past, so epoch > lim(C) along the path (E) and x is in
//!    PB(C).
//! So D(B) sums over { x in outlier(B) : x in past(C) + C }. Those x have
//! epoch(x) > lim(B), and by the same path argument they are exactly the
//! members of W with epoch > lim(B) (all of W has that, except C itself, and
//! epoch(C) >= epoch(B) > lim(B)). Finally F(B) restricted to W is computed
//! with a forward search from B that never leaves W: a forward path from B to
//! x in W runs through blocks y with B in past(y) and y in past(x), so
//! epoch(y) >= epoch(B) > lim(B) and y in past(C) + C, hence y in W. PB(B) is
//! computed exactly as the brute force does. The weight of each block is the
//! same `block_weight`, and the sum of a set does not depend on order.

use super::{ConsensusGraphInner, NULL};
use crate::block_data_manager::NULLU64;
use mazze_types::{U256, U512};
use std::collections::{HashSet, VecDeque};

lazy_static! {
    /// `MAZZE_OUTLIER_CROSSCHECK=1`: every windowed outlier difficulty is
    /// recomputed with the arena-wide brute force and the node panics on a
    /// difference. Debug aid only; it restores the O(arena) cost.
    pub static ref OUTLIER_CROSSCHECK: bool =
        std::env::var("MAZZE_OUTLIER_CROSSCHECK")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false);
}

impl ConsensusGraphInner {
    /// `lim(me)` of `compute_outlier_bruteforce`; `None` for a block without a
    /// parent (the brute force returns an empty outlier set for it).
    fn outlier_epoch_limit(&self, me: usize) -> Option<u64> {
        let parent = self.arena[me].parent;
        if parent == NULL {
            return None;
        }
        let mut lim = self.arena[parent].data.last_main_in_past;
        for referee in &self.arena[me].referees {
            lim =
                std::cmp::max(lim, self.arena[*referee].data.last_main_in_past);
        }
        Some(lim)
    }

    /// Blocks reached from `start` over parent/referee edges through blocks
    /// with `epoch_number > lim`, plus `start`: the first phase of
    /// `compute_outlier_bruteforce`.
    fn outlier_past_window(&self, start: usize, lim: u64) -> HashSet<usize> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        visited.insert(start);
        queue.push_back(start);
        while let Some(index) = queue.pop_front() {
            let parent = self.arena[index].parent;
            if parent != NULL
                && self.arena[parent].data.epoch_number > lim
                && visited.insert(parent)
            {
                queue.push_back(parent);
            }
            for referee in &self.arena[index].referees {
                if self.arena[*referee].data.epoch_number > lim
                    && visited.insert(*referee)
                {
                    queue.push_back(*referee);
                }
            }
        }
        visited
    }

    fn outlier_counted_weight(&self, x: usize) -> U512 {
        U512::from(U256::from(self.block_weight(x)))
    }

    /// Outlier difficulty of epoch block `b` (epoch of main block `main`) with
    /// cutoff main block `cutoff`, from the outlier sets as the executor got
    /// them (cache or brute force). This is the reference definition.
    pub(crate) fn outlier_difficulty_from_sets(
        &self, b_outlier: &HashSet<usize>, cutoff_outlier: &HashSet<usize>,
        main: usize,
    ) -> U512 {
        let mut d: U512 = 0.into();
        for idx in b_outlier {
            if self.is_same_era(*idx, main) && !cutoff_outlier.contains(idx) {
                d += self.outlier_counted_weight(*idx);
            }
        }
        d
    }

    /// Outlier difficulty from the brute-force outlier sets of `b` and
    /// `cutoff` (both O(arena)).
    pub(crate) fn outlier_difficulty_bruteforce(
        &self, b: usize, cutoff: usize, main: usize,
    ) -> U512 {
        use super::consensus_new_block_handler::ConsensusNewBlockHandler;
        let b_set =
            ConsensusNewBlockHandler::compute_outlier_hashset_bruteforce(
                self, b,
            );
        let c_set =
            ConsensusNewBlockHandler::compute_outlier_hashset_bruteforce(
                self, cutoff,
            );
        self.outlier_difficulty_from_sets(&b_set, &c_set, main)
    }

    /// The same value as `outlier_difficulty_bruteforce(b, cutoff, main)`,
    /// computed inside the epoch window (lim(b), epoch(cutoff)]. See the
    /// module documentation for the proof. `None` when a precondition of the
    /// proof does not hold; the caller then uses the brute force.
    pub(crate) fn outlier_difficulty_windowed(
        &self, b: usize, cutoff: usize, main: usize,
    ) -> Option<U512> {
        let lim_b = self.outlier_epoch_limit(b)?;
        let lim_c = self.outlier_epoch_limit(cutoff)?;
        let e_b = self.arena[b].data.epoch_number;
        let e_c = self.arena[cutoff].data.epoch_number;
        let era_h = self.cur_era_genesis_height;
        if e_b == NULLU64
            || e_c == NULLU64
            || !(lim_b < e_b && e_b <= e_c && lim_c < e_c)
            || lim_b < era_h
            || lim_c < era_h
        {
            return None;
        }
        let c_main_index = self.height_to_main_index(e_c);
        if c_main_index >= self.main_chain.len()
            || self.main_chain[c_main_index] != cutoff
        {
            return None;
        }

        let window = self.outlier_past_window(cutoff, lim_b);
        let past_b = self.outlier_past_window(b, lim_b);

        // F(b) inside the window.
        let mut future_b = HashSet::new();
        let mut queue = VecDeque::new();
        future_b.insert(b);
        queue.push_back(b);
        while let Some(index) = queue.pop_front() {
            for next in self.arena[index]
                .children
                .iter()
                .chain(self.arena[index].referrers.iter())
            {
                if window.contains(next) && future_b.insert(*next) {
                    queue.push_back(*next);
                }
            }
        }

        let mut d: U512 = 0.into();
        for x in &window {
            let x = *x;
            let node = &self.arena[x];
            if node.data.epoch_number > lim_b
                && !past_b.contains(&x)
                && !future_b.contains(&x)
                && (node.data.activated
                    || node.data.inactive_dependency_cnt == NULL)
                && node.era_block != NULL
                && self.is_same_era(x, main)
            {
                d += self.outlier_counted_weight(x);
            }
        }
        Some(d)
    }

    /// Test/debug helper: for every main block with reward information,
    /// compare the windowed and brute-force outlier difficulty of each of its
    /// executable epoch blocks. Panics on a difference.
    pub fn check_outlier_difficulty_windowed(&self) -> OutlierCheckStats {
        let mut st = OutlierCheckStats::default();
        st.era_genesis_height = self.cur_era_genesis_height;
        for (_, node) in self.arena.iter() {
            if node.data.partial_invalid {
                st.partial_invalid += 1;
            }
            if !node.data.activated {
                st.inactive += 1;
            }
            if node.data.epoch_number == NULLU64 {
                st.no_epoch += 1;
            }
            if node.era_block == NULL {
                st.out_era += 1;
            }
        }
        for main_index in 0..self.main_chain.len() {
            let me = self.main_chain[main_index];
            let (main, cutoff) = match self.get_main_reward_index(me) {
                Some(x) => x,
                None => continue,
            };
            // The era genesis' epoch list is not maintained (it may name
            // blocks already removed from the arena); it was executed before
            // it became the era genesis.
            if main == self.cur_era_genesis_block_arena_index {
                continue;
            }
            for b in self.get_ordered_executable_epoch_blocks(main).iter() {
                let brute =
                    self.outlier_difficulty_bruteforce(*b, cutoff, main);
                st.compared += 1;
                if !brute.is_zero() {
                    st.nonzero += 1;
                }
                match self.outlier_difficulty_windowed(*b, cutoff, main) {
                    Some(w) => assert_eq!(
                        w,
                        brute,
                        "outlier difficulty mismatch: block {:?} main {:?} \
                         cutoff {:?}",
                        self.arena[*b].hash,
                        self.arena[main].hash,
                        self.arena[cutoff].hash
                    ),
                    None => st.fallback += 1,
                }
            }
        }
        st
    }
}

#[derive(Debug, Default)]
pub struct OutlierCheckStats {
    /// Epoch blocks compared.
    pub compared: usize,
    /// Of which the brute force gave a non-zero outlier difficulty.
    pub nonzero: usize,
    /// Of which the windowed computation declined (brute force used).
    pub fallback: usize,
    pub partial_invalid: usize,
    /// Not activated (pending or with an inactive dependency).
    pub inactive: usize,
    /// Not in the past of the main chain (epoch NULL).
    pub no_epoch: usize,
    pub out_era: usize,
    pub era_genesis_height: u64,
}
