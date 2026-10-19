//! Multi-seed RandomX context cache shared by every PoW consumer in the node
//! (stratum share check, blockgen, sync-graph header verification, executor
//! pow_quality).
//!
//! History: the previous builder held ONE context and swapped it whenever a
//! caller asked for a different seed. Two failures followed (fleet death,
//! 2026-10-08):
//!   * Thrash — after a seed rotation the node keeps hashing headers from both
//!     sides of the boundary, and every flip rebuilt the ~2 GB full dataset.
//!     m5 produced one block per 10-40 s at difficulty 5.
//!   * Race — `update_context(seed)` and `acquire_vm()` were separate steps, so
//!     another thread could swap the context in between and the caller hashed
//!     with the WRONG seed. Observed: a nonce accepted by stratum was rejected
//!     ~9 s later by the mining thread for the same problem. Wrong hashes were
//!     also memoised in `PowComputer::pow_hash_cache` under the right key, and
//!     the same path feeds header verification and pow_quality.
//!
//! Here each seed owns its context and VM pool, and a VM is only ever used
//! with the seed it was created for. New seeds start in light mode (cheap
//! init); a seed that keeps being used is promoted to the full dataset in the
//! background. At most `MAX_FULL_DATASETS` full datasets live at once.

#![allow(dead_code)]

use parking_lot::{Mutex, RwLock};
use rust_randomx::{Context as RandomXContext, Hasher};

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
};

pub type H256 = [u8; 32];

/// Seeds kept resident: the pinned chain seeds (tip + next) plus room for the
/// previous seed (stragglers) and RPC lookups of old blocks.
const MAX_SEEDS: usize = 4;
/// Full datasets (~2 GB each) kept resident: the pinned chain seeds plus ONE
/// floating slot shared by everything else.
///
/// Pinning exists because of 2026-10-09: the explorer's RPC lookups of old
/// blocks recompute pow_quality with an old seed; once that seed crossed the
/// promotion threshold it took the full-dataset budget and demoted the TIP
/// seed, so tip verification ping-ponged between full and light mode.
const MAX_FULL_DATASETS: usize = 3;
/// Hashes on a light-mode seed before it is promoted to a full dataset.
/// Stragglers from the previous seed epoch stay well below this; the tip seed
/// (or the seed being replayed in catch-up) crosses it within seconds.
const PROMOTE_AFTER_HASHES: u64 = 64;

/// TEST-ONLY: `MAZZE_RANDOMX_LIGHT_ONLY=1` keeps every seed in light mode
/// (no ~2 GB full datasets). Used by the local multi-node harness
/// (scripts/local-restart-repro.sh) so several nodes fit on one machine.
/// Hashes are identical; only speed differs. Never set this in production.
fn light_only() -> bool {
    static LIGHT_ONLY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LIGHT_ONLY.get_or_init(|| {
        std::env::var("MAZZE_RANDOMX_LIGHT_ONLY")
            .map_or(false, |v| v == "1" || v == "true")
    })
}

struct SeedSlot {
    seed: H256,
    context: RwLock<Arc<RandomXContext>>,
    is_full: AtomicBool,
    promoting: AtomicBool,
    vms: Mutex<Vec<Hasher>>,
    hashes: AtomicU64,
    last_used: AtomicU64,
}

impl SeedSlot {
    fn new(seed: H256, full: bool) -> Self {
        SeedSlot {
            seed,
            context: RwLock::new(Arc::new(RandomXContext::new(&seed, full))),
            is_full: AtomicBool::new(full),
            promoting: AtomicBool::new(false),
            vms: Mutex::new(Vec::new()),
            hashes: AtomicU64::new(0),
            last_used: AtomicU64::new(0),
        }
    }
}

pub struct RandomXCacheBuilder {
    slots: RwLock<HashMap<H256, Arc<SeedSlot>>>,
    /// Serialises slot creation so concurrent callers for a new seed build its
    /// context once.
    create_lock: Mutex<()>,
    /// Seed of the most recently created slot; reported by `get_seed_hash`.
    latest_seed: RwLock<H256>,
    /// Seeds the chain needs now (tip seed, next seed). Never demoted or
    /// evicted; set by consensus via `set_chain_seeds`.
    pinned: RwLock<Vec<H256>>,
    tick: AtomicU64,
}

pub struct VMHandle {
    vm: Option<Hasher>,
    slot: Arc<SeedSlot>,
}

impl RandomXCacheBuilder {
    pub fn new(seed_hash: H256) -> Arc<Self> {
        let builder = Arc::new(RandomXCacheBuilder {
            slots: RwLock::new(HashMap::new()),
            create_lock: Mutex::new(()),
            latest_seed: RwLock::new(seed_hash),
            pinned: RwLock::new(vec![seed_hash]),
            tick: AtomicU64::new(0),
        });
        // The startup seed is the tip seed in steady state: build it full.
        let slot = Arc::new(SeedSlot::new(seed_hash, !light_only()));
        builder.slots.write().insert(seed_hash, slot);
        builder
    }

    pub fn get_seed_hash(&self) -> H256 { *self.latest_seed.read() }

    /// Returns a VM bound to `seed_hash`. The returned VM's context key is
    /// guaranteed to equal `seed_hash`.
    pub fn get_vm_handler(self: &Arc<Self>, seed_hash: &H256) -> VMHandle {
        let slot = self.slot_for(seed_hash);
        let now = self.tick.fetch_add(1, Ordering::Relaxed);
        slot.last_used.store(now, Ordering::Relaxed);
        let used = slot.hashes.fetch_add(1, Ordering::Relaxed) + 1;
        if used >= PROMOTE_AFTER_HASHES && !slot.is_full.load(Ordering::Acquire)
        {
            self.maybe_promote(&slot);
        }

        let context = slot.context.read().clone();
        let pooled = {
            let mut vms = slot.vms.lock();
            let mut found = None;
            while let Some(vm) = vms.pop() {
                // VMs built before a promotion still point at the light
                // context; they are correct but slow, so drop them.
                if same_context(&vm, &context) {
                    found = Some(vm);
                    break;
                }
            }
            found
        };
        let vm = pooled.unwrap_or_else(|| Hasher::new(context));
        assert_eq!(
            vm.context().key(),
            &seed_hash[..],
            "RandomX VM bound to the wrong seed"
        );
        VMHandle {
            vm: Some(vm),
            slot,
        }
    }

    /// Build `seed_hash`'s full dataset ahead of time (the next RandomX epoch's
    /// seed is known a whole epoch in advance), so the rotation never runs in
    /// light mode. Never blocks the caller.
    pub fn prewarm(self: &Arc<Self>, seed_hash: &H256) {
        if let Some(slot) = self.slots.read().get(seed_hash) {
            if slot.is_full.load(Ordering::Acquire)
                || slot.promoting.load(Ordering::Acquire)
            {
                return;
            }
        }
        let builder = self.clone();
        let seed = *seed_hash;
        let _ = thread::Builder::new()
            .name("randomx-prewarm".into())
            .spawn(move || {
                let slot = builder.slot_for(&seed);
                builder.maybe_promote(&slot);
            });
    }

    /// Pin the seeds the chain currently needs and build their full datasets
    /// in the background. Unpinned seeds keep working, just in light mode or
    /// in the single floating full slot.
    pub fn set_chain_seeds(self: &Arc<Self>, current: &H256, next: Option<&H256>) {
        {
            let mut pinned = self.pinned.write();
            pinned.clear();
            pinned.push(*current);
            if let Some(n) = next {
                pinned.push(*n);
            }
        }
        self.prewarm(current);
        if let Some(n) = next {
            self.prewarm(n);
        }
    }

    fn is_pinned(&self, seed: &H256) -> bool {
        self.pinned.read().contains(seed)
    }

    fn slot_for(&self, seed_hash: &H256) -> Arc<SeedSlot> {
        if let Some(slot) = self.slots.read().get(seed_hash) {
            return slot.clone();
        }
        let _guard = self.create_lock.lock();
        if let Some(slot) = self.slots.read().get(seed_hash) {
            return slot.clone();
        }
        info!("Creating light RandomX context for seed {:?}", seed_hash);
        // Built outside the `slots` lock: other seeds keep hashing meanwhile.
        let slot = Arc::new(SeedSlot::new(*seed_hash, false));
        slot.last_used
            .store(self.tick.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
        let mut slots = self.slots.write();
        while slots.len() >= MAX_SEEDS {
            let lru = slots
                .values()
                .filter(|s| !self.is_pinned(&s.seed))
                .min_by_key(|s| s.last_used.load(Ordering::Relaxed))
                .map(|s| s.seed);
            match lru {
                // In-flight VMHandles keep their slot alive via Arc.
                Some(lru) => {
                    slots.remove(&lru);
                }
                None => break,
            }
        }
        slots.insert(*seed_hash, slot.clone());
        *self.latest_seed.write() = *seed_hash;
        slot
    }

    fn maybe_promote(self: &Arc<Self>, slot: &Arc<SeedSlot>) {
        if light_only() {
            return;
        }
        if slot.promoting.swap(true, Ordering::AcqRel) {
            return;
        }
        let builder = self.clone();
        let target = slot.clone();
        let spawned = thread::Builder::new()
            .name("randomx-dataset".into())
            .spawn(move || {
                let slot = target;
                let pinned = builder.is_pinned(&slot.seed);
                // Make room: only UNPINNED full seeds can be demoted (LRU
                // first). An unpinned seed that would need a pinned one
                // demoted stays light instead.
                let victims: Vec<Arc<SeedSlot>> = {
                    let slots = builder.slots.read();
                    let fulls = slots
                        .values()
                        .filter(|s| {
                            s.seed != slot.seed
                                && s.is_full.load(Ordering::Acquire)
                        })
                        .count();
                    let mut unpinned: Vec<Arc<SeedSlot>> = slots
                        .values()
                        .filter(|s| {
                            s.seed != slot.seed
                                && s.is_full.load(Ordering::Acquire)
                                && !builder.is_pinned(&s.seed)
                        })
                        .cloned()
                        .collect();
                    unpinned
                        .sort_by_key(|s| s.last_used.load(Ordering::Relaxed));
                    let excess = (fulls + 1).saturating_sub(MAX_FULL_DATASETS);
                    if !pinned && excess > unpinned.len() {
                        slot.hashes.store(0, Ordering::Relaxed);
                        slot.promoting.store(false, Ordering::Release);
                        return;
                    }
                    unpinned.into_iter().take(excess).collect()
                };
                info!(
                    "Promoting RandomX seed {:?} to full dataset (pinned={})",
                    slot.seed, pinned
                );
                let full = Arc::new(RandomXContext::new(&slot.seed, true));
                for s in victims {
                    info!("Demoting RandomX seed {:?} to light mode", s.seed);
                    let light = Arc::new(RandomXContext::new(&s.seed, false));
                    *s.context.write() = light;
                    s.vms.lock().clear();
                    s.is_full.store(false, Ordering::Release);
                    s.hashes.store(0, Ordering::Relaxed);
                    s.promoting.store(false, Ordering::Release);
                }
                *slot.context.write() = full;
                slot.vms.lock().clear();
                slot.is_full.store(true, Ordering::Release);
            });
        if spawned.is_err() {
            warn!("Failed to spawn RandomX dataset thread; staying light");
            slot.promoting.store(false, Ordering::Release);
        }
    }

}

/// A pooled VM is reusable only if it was built on the slot's current
/// context (VMs created before a promotion/demotion are discarded).
fn same_context(vm: &Hasher, context: &Arc<RandomXContext>) -> bool {
    std::ptr::eq(vm.context(), Arc::as_ptr(context))
}

impl VMHandle {
    pub fn get_vm(&self) -> &Hasher {
        self.vm.as_ref().expect("VM should exist")
    }
}

impl Drop for VMHandle {
    fn drop(&mut self) {
        if let Some(vm) = self.vm.take() {
            let context = self.slot.context.read().clone();
            if same_context(&vm, &context) {
                self.slot.vms.lock().push(vm);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn seed(b: u8) -> H256 { [b; 32] }

    fn is_full(builder: &RandomXCacheBuilder, s: &H256) -> bool {
        builder
            .slots
            .read()
            .get(s)
            .map_or(false, |slot| slot.is_full.load(Ordering::Acquire))
    }

    fn wait_until(mut f: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(300) {
            if f() {
                return true;
            }
            thread::sleep(Duration::from_millis(200));
        }
        false
    }

    fn hammer(builder: &Arc<RandomXCacheBuilder>, s: &H256) {
        for i in 0..(PROMOTE_AFTER_HASHES * 2) {
            let h = builder.get_vm_handler(s);
            h.get_vm().hash(&[i as u8; 64]);
        }
    }

    /// Regression for 2026-10-09: RPC lookups of old blocks (unpinned seeds)
    /// must never demote the pinned tip/next seeds.
    #[test]
    fn pinned_seeds_survive_old_seed_traffic() {
        let (tip, next, old1, old2) = (seed(1), seed(2), seed(3), seed(4));
        let builder = RandomXCacheBuilder::new(tip);
        builder.set_chain_seeds(&tip, Some(&next));
        assert!(wait_until(|| is_full(&builder, &tip) && is_full(&builder, &next)));

        // First old seed takes the floating full slot.
        hammer(&builder, &old1);
        assert!(wait_until(|| is_full(&builder, &old1)));
        // Second old seed may only displace the first, never a pinned one.
        hammer(&builder, &old2);
        assert!(wait_until(|| is_full(&builder, &old2) && !is_full(&builder, &old1)));
        assert!(is_full(&builder, &tip), "tip seed was demoted");
        assert!(is_full(&builder, &next), "next seed was demoted");
        assert!(builder.slots.read().len() <= MAX_SEEDS);
    }
}
