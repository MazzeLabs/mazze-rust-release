// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Windowed execution profile (timing only, no effect on what is
//! computed or stored). `compute_epoch` reports the wall time of each of
//! its phases; every `WINDOW_EPOCHS` epochs one `exec-profile` line logs
//! the per-epoch averages over the window together with the epoch rate
//! and the MDBX write transactions / fsyncs the window cost, so a
//! catching-up node shows where its time goes without a profiler.

use mazze_storage::mdbx_rw_stats;
use parking_lot::Mutex;
use std::time::{Duration, Instant};

const WINDOW_EPOCHS: u64 = 256;

/// Phases of `compute_epoch`, in order.
#[derive(Clone, Copy)]
pub(crate) enum Phase {
    /// Block-number index writes and the block bodies lookup.
    Pre = 0,
    NewState = 1,
    Evm = 2,
    RewardPow = 3,
    Commit = 4,
    /// Epoch execution commitment insert and bookkeeping.
    Post = 5,
    /// Commit of the epoch's buffered block-data writes.
    Flush = 6,
}
const NUM_PHASES: usize = 7;
const PHASE_NAMES: [&str; NUM_PHASES] = [
    "pre",
    "new_state",
    "evm",
    "reward+pow",
    "commit",
    "post",
    "flush",
];

struct Window {
    started: Instant,
    rw_start: mdbx_rw_stats::Snapshot,
    epochs: u64,
    blocks: u64,
    txs: u64,
    phase: [Duration; NUM_PHASES],
}

impl Window {
    fn new() -> Self {
        Window {
            started: Instant::now(),
            rw_start: mdbx_rw_stats::snapshot(),
            epochs: 0,
            blocks: 0,
            txs: 0,
            phase: [Duration::ZERO; NUM_PHASES],
        }
    }
}

lazy_static! {
    static ref WINDOW: Mutex<Option<Window>> = Mutex::new(None);
}

/// Per-epoch phase timer handed through `compute_epoch`.
pub(crate) struct EpochTimer {
    last: Instant,
    phase: [Duration; NUM_PHASES],
}

impl EpochTimer {
    pub fn start() -> Self {
        EpochTimer {
            last: Instant::now(),
            phase: [Duration::ZERO; NUM_PHASES],
        }
    }

    /// Charge the time since the previous mark to `phase`.
    pub fn mark(&mut self, phase: Phase) {
        let now = Instant::now();
        self.phase[phase as usize] += now - self.last;
        self.last = now;
    }

    pub fn finish(self, blocks: usize, txs: usize) {
        let mut guard = WINDOW.lock();
        let w = guard.get_or_insert_with(Window::new);
        w.epochs += 1;
        w.blocks += blocks as u64;
        w.txs += txs as u64;
        for i in 0..NUM_PHASES {
            w.phase[i] += self.phase[i];
        }
        if w.epochs < WINDOW_EPOCHS {
            return;
        }
        let wall = w.started.elapsed();
        let rw = mdbx_rw_stats::snapshot().delta(&w.rw_start);
        let n = w.epochs as f64;
        let mut phases = String::new();
        let mut in_compute = Duration::ZERO;
        for i in 0..NUM_PHASES {
            in_compute += w.phase[i];
            phases.push_str(&format!(
                " {}={:.2}",
                PHASE_NAMES[i],
                w.phase[i].as_secs_f64() * 1e3 / n
            ));
        }
        info!(
            "exec-profile epochs={} wall={:.1}s rate={:.2}ep/s \
             blocks/ep={:.2} txs/ep={:.2} | ms/ep:{} outside={:.2} \
             | mdbx/ep: rw_txns={:.1} rw_ms={:.2} wops={:.1} fsync={:.2} \
             msync={:.2} [{}]",
            w.epochs,
            wall.as_secs_f64(),
            n / wall.as_secs_f64().max(1e-9),
            w.blocks as f64 / n,
            w.txs as f64 / n,
            phases,
            wall.saturating_sub(in_compute).as_secs_f64() * 1e3 / n,
            rw.total_count() as f64 / n,
            rw.total_nanos() as f64 / 1e6 / n,
            rw.wops as f64 / n,
            rw.fsync as f64 / n,
            rw.msync as f64 / n,
            rw.describe(),
        );
        *guard = Some(Window::new());
    }
}
