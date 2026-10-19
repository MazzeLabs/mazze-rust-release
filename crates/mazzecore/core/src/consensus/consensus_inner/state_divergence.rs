// Copyright 2026 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Detects a node whose own execution state has diverged from the network.
//!
//! Execution is deterministic, but a node that ever computes one epoch
//! differently (a block that never reached its consensus, a lost body, a
//! storage fault, ...) stays different forever: every later state builds on
//! the wrong one, and the reward inputs themselves feed back on it --
//! `no_reward` of an epoch's main block is `!state_valid`, i.e. "does the
//! block's deferred state root match OURS". Such a node blames every block
//! of the other miners, and they blame every block it mines, which is
//! exactly the "≥2 miners => every node logs `state/blame is invalid` for a
//! large share of blocks" pattern. Nothing in the protocol repairs it; the
//! node has to be resynced.
//!
//! An honest node, in contrast, only rejects the blocks of the diverged
//! miner(s). The main chain follows the heaviest subtree, so as long as the
//! honest miners hold the majority of the block production the honest
//! node's main chain is mostly state-valid. When most of OUR main chain is
//! state-invalid in our own view, the node is the outlier. This tracker only
//! observes and reports (log + gauge + `mazze_consensusDebug`); it changes no
//! consensus or mining behaviour.

use metrics::{Gauge, GaugeUsize};
use mazze_types::Address;
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

lazy_static! {
    static ref LOCAL_STATE_DIVERGED: Arc<dyn Gauge<usize>> =
        GaugeUsize::register_with_group(
            "system_metrics",
            "local_state_diverged"
        );
}

/// Number of recent main-chain state verdicts considered.
const WINDOW: usize = 256;
/// Report divergence when more than this share of the window is invalid.
const DIVERGED_INVALID_PERCENT: usize = 67;
/// Clear the report again below this share (only expected after a resync).
const RECOVERED_INVALID_PERCENT: usize = 10;
const REPORT_EVERY: Duration = Duration::from_secs(600);

#[derive(Default)]
pub struct StateDivergenceTracker {
    /// (height, author, state_valid) of recent main-chain verdicts.
    window: VecDeque<(u64, Address, bool)>,
    invalid: usize,
    /// Height of the first invalid verdict of the current divergent stretch.
    diverged_since: Option<u64>,
    last_report: Option<Instant>,
}

impl StateDivergenceTracker {
    /// Record the `state_valid` verdict of a block that is on the local
    /// main chain at the time it is computed.
    pub fn record(&mut self, height: u64, author: Address, state_valid: bool) {
        self.window.push_back((height, author, state_valid));
        if !state_valid {
            self.invalid += 1;
        }
        if self.window.len() > WINDOW {
            if let Some((_, _, valid)) = self.window.pop_front() {
                if !valid {
                    self.invalid -= 1;
                }
            }
        }
        if self.window.len() < WINDOW {
            return;
        }
        let pct = self.invalid * 100 / self.window.len();
        if pct > DIVERGED_INVALID_PERCENT {
            if self.diverged_since.is_none() {
                self.diverged_since = self
                    .window
                    .iter()
                    .find(|(_, _, valid)| !*valid)
                    .map(|(h, _, _)| *h);
                LOCAL_STATE_DIVERGED.update(1);
            }
            let due = self
                .last_report
                .map_or(true, |t| t.elapsed() >= REPORT_EVERY);
            if due {
                self.last_report = Some(Instant::now());
                error!(
                    "LOCAL STATE DIVERGED: {}% of the last {} main-chain \
                     blocks have a deferred state/blame that disagrees with \
                     this node's own execution (first seen near height {:?}; \
                     invalid/total by author: {}). The network's miners agree \
                     with each other, not with this node: its state is wrong \
                     and will not recover on its own. Stop mining on this \
                     node and resync it (wipe the chain data, or fast-sync \
                     from a healthy peer).",
                    pct,
                    self.window.len(),
                    self.diverged_since,
                    self.author_summary(),
                );
            }
        } else if pct < RECOVERED_INVALID_PERCENT
            && self.diverged_since.is_some()
        {
            self.diverged_since = None;
            LOCAL_STATE_DIVERGED.update(0);
            warn!(
                "local state agrees with the main chain again ({}% invalid)",
                pct
            );
        }
    }

    pub fn diverged_since(&self) -> Option<u64> { self.diverged_since }

    /// "invalid/total" share of the window, e.g. "12/256".
    pub fn invalid_share(&self) -> String {
        format!("{}/{}", self.invalid, self.window.len())
    }

    fn author_summary(&self) -> String {
        let mut by_author: HashMap<Address, (usize, usize)> = HashMap::new();
        for (_, author, valid) in &self.window {
            let e = by_author.entry(*author).or_default();
            if !*valid {
                e.0 += 1;
            }
            e.1 += 1;
        }
        let mut v: Vec<_> = by_author.into_iter().collect();
        v.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
        v.iter()
            .map(|(a, (inv, tot))| format!("{:?}={}/{}", a, inv, tot))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(t: &mut StateDivergenceTracker, n: u64, f: impl Fn(u64) -> (u8, bool)) {
        for h in 0..n {
            let (a, valid) = f(h);
            t.record(h, Address::from_low_u64_be(a as u64), valid);
        }
    }

    #[test]
    fn honest_node_with_one_bad_minority_miner_is_not_flagged() {
        let mut t = StateDivergenceTracker::default();
        // Miner 3 (a third of the blocks) publishes wrong roots.
        feed(&mut t, 1000, |h| ((h % 3) as u8, h % 3 != 2));
        assert_eq!(t.diverged_since(), None);
    }

    #[test]
    fn diverged_node_is_flagged_and_cleared_after_resync() {
        let mut t = StateDivergenceTracker::default();
        // Own blocks (author 0, a quarter) look fine, everyone else's not.
        feed(&mut t, 300, |h| ((h % 4) as u8, h % 4 == 0));
        assert!(t.diverged_since().is_some());
        // After a resync everything agrees again.
        feed(&mut t, 300, |h| ((h % 4) as u8, true));
        assert_eq!(t.diverged_since(), None);
    }

    #[test]
    fn not_flagged_before_the_window_is_full() {
        let mut t = StateDivergenceTracker::default();
        feed(&mut t, (WINDOW - 1) as u64, |_| (1, false));
        assert_eq!(t.diverged_since(), None);
    }
}
