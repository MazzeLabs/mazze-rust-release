# Fast-Sync Design — Joining Nodes & Long Chains

**Status:** Design only. No code change yet. Recommended reading order: [chain-model.md](chain-model.md) §3 (PoW), [checkpoint-snapshot-lifecycle.md](checkpoint-snapshot-lifecycle.md), then this doc.

## 1. Problem

A node joining the live mesh starts at genesis and must reach the tip. Empirically on the current testnet (2026-05-26):

| Quantity | Value |
| --- | --- |
| Chain growth | ~200 epochs/min |
| Local header sync rate | ~470 epochs/min |
| Net catch-up rate | ~270 epochs/min |
| Time to catch up a 6 h chain (72k epochs) | ~4.5 h |
| Time to catch up a 30 d chain (8.6M epochs) | ~13 d |

Catch-up is asymptotic — every hour we spend syncing, the tip grows another ~12k epochs. With a sufficiently large chain age, **a fresh node cannot catch up at all** with the current sync mechanism.

## 2. Why it's slow — corrected diagnosis

Initial guess was RandomX validation per header. **That was wrong.** PoW verification is *already* skipped during catch-up:

```text
crates/mazzecore/core/src/verification.rs:371-374
  if self.catch_up_mode() {
      POW_SKIPPED_CATCH_UP.mark(1);
      return Ok(());
  }
```

What *is* enforced during catch-up is the **RandomX seed lookup** (defensive, prevents H-1 attack):

```text
crates/mazzecore/core/src/sync/synchronization_graph.rs:1543-1560
  match data_man.db_manager.try_get_current_seed_hash(header.height()) {
      Some(h) => h,
      None => { /* reject "seed lookup missed at height …" */ }
  }
```

That's why peers offering us tip blocks get rejected — we don't have the seed-source blocks yet. Visible in logs as:
```text
WARN Rejecting block 0x…: seed lookup missed at height 13242
     (DB inconsistency or attacker-supplied block from future epoch)
```

The actual bottleneck is **sequential header-batch download throughput**:

| Knob | Current | File |
| --- | --- | --- |
| `EPOCH_SYNC_BATCH_SIZE` | 30 | `sync/synchronization_protocol_handler.rs:118` |
| `EPOCH_SYNC_MAX_INFLIGHT` | 300 | `sync/synchronization_protocol_handler.rs:117` |
| `REQUEST_START_WAITING_TIME` | 1 s | `parameters/src/lib.rs:174` |

Theoretical max: ~150 epochs/sec across 5 peers. Observed: ~8 epochs/sec. So we're 20× off the theoretical ceiling — there's network-layer round-trip, per-peer rate-limit and probably consensus-graph insertion serialization in the path. Tuning these is a separate ticket; the *architectural* fix below makes the whole problem moot.

## 3. The architectural fix — `CatchUpCheckpoint` jump-start

The design intent is already present in the protocol:

```text
crates/mazzecore/core/src/sync/message/snapshot_manifest_request.rs:34-39
  pub struct SnapshotManifestRequest {
      pub snapshot_to_sync: SnapshotSyncCandidate,
      pub trusted_blame_block: Option<H256>,   // <-- already there
  }
```

A new node *should* be able to:
1. Identify a recent snapshot epoch hash + height (the **anchor**).
2. Skip header-syncing the chain prefix below the anchor.
3. Pull the snapshot via `SnapshotChunkSync` — state is now at the anchor.
4. Header-sync only the *tail* (anchor → tip, currently ≤ 2048 epochs).
5. Resume normal validation.

For an 11k-epoch chain that means **~2k epochs of sync work instead of 11k**. For a 1-month chain, **~2k instead of 8.6M**. The win is asymptotic.

**What's missing:** the node has no way to *get* the anchor. There's no peer-broadcast of snapshot availability, no operator config, and `get_to_sync_epoch_id()` returns local genesis for a joining node.

### 3.1 Mechanism gap — current `get_to_sync_epoch_id()`

```text
crates/mazzecore/core/src/consensus/consensus_inner/mod.rs:3419-3447
pub fn get_to_sync_epoch_id(&self) -> EpochId {
    let height_to_sync = self.latest_snapshot_height();
    // = cur_era_stable_height / snapshot_epoch_count * snapshot_epoch_count
    //   ↑ for a joining node this is 0 → returns genesis
}
```

And the gate in `catch_up_completed()`:

```text
crates/mazzecore/core/src/consensus/mod.rs:2440-2474
fn catch_up_completed(&self, peer_median_epoch: u64) -> bool {
    let sync_target_height = block_height_by_hash(get_to_sync_epoch_id()); // 0
    if self.best_epoch_number() < sync_target_height { return false; }      // ok (0 ≤ ours)
    if sync_target_height + snapshot_epoch_count < peer_median_epoch {      // 0 + 2048 < 11000 → true
        if let Some(gap) = self.config.sync_state_epoch_gap { … }
        else { return false; }                                              // ← blocks transition
    }
    true
}
```

→ A joining node can never enter `CatchUpCheckpoint` until its *own* `cur_era_stable_height` advances close to the peer median, which can only happen via the slow linear header sync we're trying to avoid.

## 4. Design options

### Option A — Operator-provided trusted checkpoint (RECOMMENDED for MVP)

**Trust model:** operator obtains a known-good `(epoch_height, block_hash)` pair from a trusted source (release artifact, governance signature, polling N peers). Configures it in `hydra.toml`.

**Changes:**
1. Add `trusted_checkpoint_height: Option<u64>` + `trusted_checkpoint_hash: Option<H256>` to `ConsensusConfig` and the client config.
2. When **both** are set **and** local `cur_era_stable_height == 0`:
   - `get_to_sync_epoch_id()` returns `trusted_checkpoint_hash`.
   - `catch_up_completed()` returns `true` immediately — the node is "ready" to checkpoint-sync without any local header history.
   - The seed-lookup-missed rejection in `synchronization_graph.rs:1543` accepts headers below `trusted_checkpoint_height` without a seed (the trusted checkpoint is the anchor of trust).
3. `CatchUpCheckpointPhase::start()` issues a `SnapshotManifestRequest` for the trusted hash, with `trusted_blame_block` populated.
4. Once snapshot sync completes, normal flow resumes — state is at the anchor, the node header-syncs only the tail.

**Pros:** small, surgical change. Trust model is explicit and opt-in. Operators always control what they trust. Compatible with existing `SnapshotManifestRequest.trusted_blame_block`.

**Cons:** requires out-of-band distribution of the checkpoint. UX falls on operators.

### Option B — Peer-quorum-derived checkpoint

**Trust model:** node queries N peers for their latest snapshot hash; if ≥ M (e.g., 4 of 5) agree, treat as trusted anchor.

**Changes:** all of Option A's plumbing **plus**:
1. Extend `StatusV3` → `StatusV4` with `latest_snapshot_epoch_number: Option<u64>` and `latest_snapshot_epoch_hash: Option<H256>`.
2. Sync layer aggregates peer reports.
3. Reach quorum → seed the same path as Option A.

**Pros:** zero operator config. Works against any honest-majority mesh.

**Cons:**
- Subject to majority-collusion attack. Mesh must be sufficiently honest at boot time.
- Wire-format change (Status message bump) — needs careful version negotiation.
- Bigger surface area for review.

### Option C — Light-client header sync

Skip seed validation entirely below a configurable height, trust the snapshot's chain-prefix proofs. Even more aggressive than B. Most invasive. **Defer.**

## 5. Recommended path

Implement **Option A** first. It's small, additive, opt-in, and unlocks the live testnet for any reasonable joiner *immediately* (we publish the latest checkpoint in release notes / a signed file). It also exercises the trusted-anchor codepath end-to-end, so layering Option B on top later is straightforward.

### 5.1 Implementation plan (Option A)

```text
config:
  ConsensusConfig::trusted_checkpoint_height: Option<u64>
  ConsensusConfig::trusted_checkpoint_hash:   Option<H256>
  ClientConfig:    same, plumbed through

consensus_inner/mod.rs:get_to_sync_epoch_id()
  if let (Some(h), Some(hash)) = config.trusted_checkpoint_(...) {
      if cur_era_stable_height == 0 { return hash }
  }
  // … existing logic

consensus/mod.rs:catch_up_completed()
  if config.trusted_checkpoint_hash.is_some()
     && cur_era_stable_height == 0 {
      return true;   // ready to enter CatchUpCheckpoint immediately
  }
  // … existing logic

synchronization_graph.rs:1543  (the seed-lookup rejection)
  if let Some(h) = config.trusted_checkpoint_height {
      if header.height() < h && cur_era_stable_height == 0 {
          // pre-anchor headers are trusted by virtue of the anchor.
          // We won't actually receive these from peers in the new
          // flow, but if a stray header shows up, accept without seed.
          return /* don't reject */;
      }
  }

CatchUpCheckpointPhase::start() — already requests the manifest for
get_to_sync_epoch_id().  Set trusted_blame_block = trusted_checkpoint_hash
when set.
```

### 5.2 Test plan

1. Unit tests for the new config gate + epoch-id override.
2. Local-node integration test:
   - Stand up the 5-node fleet at e.g. 5k epochs.
   - Set `trusted_checkpoint_height/hash` in `hydra.local.toml` to the fleet's latest snapshot (epoch 4096, say).
   - Start local node with `execute_genesis = false`.
   - Verify it enters `CatchUpCheckpoint` within seconds (not minutes), pulls the snapshot, joins as Normal in < 2 min.
3. Negative test: wrong hash → snapshot validation fails → node refuses to sync. Confirm we surface a clear error, not silent corruption.
4. State-test regression: full 34036 corpus must still pass.
5. Native + eSpace transfer tests against the local node post-sync.

### 5.3 Operational hand-off

Once landed:
- Publish a `trusted-checkpoints.json` artifact alongside each release: `{ height, block_hash, signed_by }`.
- `setup-guide.md` adds a section: "Joining mid-chain".
- Optional: `mazze --get-trusted-checkpoint <rpc-url>` helper subcommand that polls a node and prints the suggested config block.

### 5.5 Implementation status (2026-05-26 session 2)

Scaffolding landed (Option A foundation, ~240 LOC across 9 files), gated entirely behind `has_trusted_checkpoint()` so the normal path is unchanged:

| Component | Status | Notes |
| --- | --- | --- |
| `ConsensusConfig` fields (4) + plumbing through `ClientConfig` / raw TOML | ✅ | Both `trusted_checkpoint_*` and `trusted_blame_*` pairs required by `has_trusted_checkpoint()`. |
| `get_to_sync_epoch_id()` override returns trusted checkpoint hash when fresh | ✅ | Verified live: phase transition is instant (~1 s). |
| `catch_up_completed()` shortcut for fresh trusted-checkpoint node | ✅ | Without this the gate would never open. |
| `CatchUpSyncBlockHeader.next()` skips `normal_peers_share_known_terminal` for trusted-checkpoint case | ✅ | We deliberately don't ingest peer headers, so terminals never appear in graph. |
| Seed-lookup gate: pre-anchor headers accepted with zero seed; post-anchor headers `TemporarySkipped` (peer not killed) | ✅ | Critical — the first iteration returned `Invalid` and killed all 5 peers in seconds. |
| `get_trusted_blame_block_for_snapshot()` override returns the operator-provided `trusted_blame_hash` | ✅ | Snapshot ≠ blame anchor; protocol requires blame anchor at least N epochs ABOVE snapshot. |
| `start_sync_for_candidate` height fallback to operator-supplied height when local header is missing | ✅ | The standard path `.expect("Syncing checkpoint should have available header")` was the next panic. |
| `validate_blame_states` graceful failure when local headers missing | ⚠️  Soft-handled | Previously panicked; now returns `None` with an operator-facing warning. Snapshot completion then drops into the existing `SNAPSHOT-SYNC FALLBACK` path → genesis replay. |
| **End-to-end snapshot completion under trusted-checkpoint** | ❌ Not yet | See §5.6. |

### 5.6 What's still missing (≈ another full session of work)

The protocol's snapshot validation chain assumes the **anchor block header AND the trusted-blame block header are locally present** before the manifest response is processed (`snapshot_manifest_manager.rs:316–327`). They aren't, under fast-sync — we deliberately skipped header sync to that height.

To complete Option A robustly, **one** of the following is needed:

1. **Header-anchor pre-fetch + orphan-persistence fix:** before issuing the `SnapshotManifestRequest`, the local node does a *targeted* `GetBlockHeaders` for exactly the two anchor block hashes (snapshot + blame). Headers are inserted into the local graph with the zero-seed allowance. **HOWEVER — attempted in this session, and the headers do arrive at the local node but are not persisted to `data_man` because they are orphans (their parent hashes aren't in our graph) and the persistence gate at `synchronization_graph.rs:1480-1488` only fires for non-orphan headers.** Fixing this means **either** also fetching the parents recursively (chain-prefix sync — explodes scope), **or** adding a special "trusted orphan" persistence path that writes the operator-vouched headers directly to `data_man` without going through the normal not-ready-frontier dance. Plus then validate_blame_states does its own internal walks that ALSO assume chain context, which would need to be either short-circuited or fed via the same operator-vouched data path.
2. **Snapshot transport carries the anchor headers (and the full chain context):** extend `SnapshotManifestResponse` (V5) with `snapshot_header`, `trusted_blame_header`, plus the chain headers between them (or a Merkle proof of state-root validity). Validate against configured hashes on receipt, then bypass the local-header lookups in `validate_blame_states`. Wire-format change → version negotiation.
3. **Rework `validate_blame_states` and adjacent functions to work without local headers**, using only the operator-supplied heights + the on-wire response data. Refactor `snapshot_manifest_manager.rs:303–410` plus the downstream chunk processing. Doesn't require a protocol change but is the structurally deepest path because the assumptions are spread across multiple modules.

**Recommendation:** option (2) is probably the most robust because it makes the trust delegation explicit on the wire — the operator's anchor pair is the trust input, and the response carries everything needed to verify against it. Option (1) is appealing for its size but the orphan-persistence + downstream-walk issues compound. Option (3) is a big refactor.

**Estimate:** ≈ 200–400 LOC across 4–6 files, plus a real test for "wrong hash fails loudly" before shipping.

### 5.7 Verified during this session

- `CatchUpSyncBlockHeader → CatchUpCheckpoint` transition fires in ~1 s when trusted-checkpoint is configured (down from ~30+ min linear sync).
- All 5 peers stay connected (no `InvalidBlock` kills — `TemporarySkipped` defers post-anchor headers cleanly).
- `SnapshotManifestRequest` is issued with `trusted_blame_block = trusted_blame_hash` populated (separate from snapshot hash — wire-protocol check `trusted_block.height() >= snapshot.height()` passes).
- Peers respond with valid manifests (e.g. `chunk_boundaries.len()=18`).
- Anchor-header pre-fetch in `CatchUpCheckpointPhase::start()` fires and the two requested headers arrive at the local node ("new block headers received" with the correct hashes).
- **Anchor-header persistence resolved (session 3)** — the actual blocker wasn't orphan persistence; it was `insert_block_header`'s `locked_for_catchup` early-return at line 1501-1504. Added a higher-priority bypass that direct-writes operator-vouched anchor hashes to `data_man` and returns `TemporarySkipped`, skipping the normal graph-insertion path entirely. Verified: `Fast-sync: persisted trusted anchor header hash=… height=16384` + `height=16884` log lines fire; `mazze_getBlockByHash` against the anchors now returns the headers.
- **Next blocker (session 3)** — `validate_blame_states` advances past the two-header check (good) but then panics at `snapshot_manifest_manager.rs:396`: a loop walks `parent_hash()` from `trusted_blame_block` (height 16884) down to the snapshot (16384), expecting all ~500 intermediate parents in `data_man`. We don't have those — that's the whole point of fast-sync. The `.expect("block header must exist")` triggers.
- The fallback flow correctly drops into the existing `SNAPSHOT-SYNC FALLBACK` path on prior iterations — no panics propagate to crash, no peer kills, no consensus corruption.
- Normal-mode operation is byte-identical: every change is behind `has_trusted_checkpoint()`. The state-test corpus and the live fleet are unaffected.

### 5.8 Why iterative piecemeal fixes hit a wall

We progressed three blockers deep in this session, each fix revealing the next architectural dependency:

1. ✅ Manifest request silently rejected → fixed by supplying a **separate blame anchor** above the snapshot.
2. ✅ Anchor headers don't reach `data_man` → fixed by **direct-persist bypass** of `locked_for_catchup`.
3. ❌ `validate_blame_states` walks the chain prefix → needs the full 500-block prefix OR a fundamentally different validation path.

Beyond (3) lie further dependencies the local fast-path doesn't satisfy:

- `validate_epoch_receipts` (immediately after) does its own chain walk for REWARD_EPOCH_COUNT epochs.
- Constructing the `RelatedData { true_state_root_by_blame_info: StateRootWithAuxInfo, snapshot_info: SnapshotInfo, parent_snapshot_info, … }` tuple that the downstream chunk processor expects requires fields like `SnapshotInfo.main_chain_parts: Vec<EpochId>` and `StateRootAuxInfo` that the local node can't compute without chain history.

The pattern: every layer downstream assumes "we already have chain context to where the snapshot lives." Trusted-anchor fast-sync flips that assumption, so each layer needs either an alternative validation path or the server has to supply that context.

### 5.9 Revised recommendation

Option (2) — extending the snapshot transport — is the right path, but it's a real protocol bump, not "~30 LOC". Concretely:

- New `SnapshotManifestResponseV5` carries: `snapshot_info: SnapshotInfo`, `parent_snapshot_info: Option<SnapshotInfo>`, `true_state_root_by_blame_info: StateRootWithAuxInfo`, plus the existing fields. The server has these already (they're what its own `validate_blame_states` would compute); it just doesn't currently ship them.
- Client: when V5 is received AND `has_trusted_checkpoint()` AND the supplied `snapshot_info.merkle_root` matches the chunks (cryptographic check, no chain context required), skip the entire `validate_blame_states` + `validate_epoch_receipts` walk and construct `RelatedData` directly from the wire data.
- Wire-format negotiation: add `SYNC_PROTO_V5` constant; legacy peers stay on V4 (with `state_sync_candidate_response` excluding fast-sync candidates).
- Fleet rollout: relaunch the 5 nodes with the V5 binary (`/tmp/relaunch.sh` already handles rsync+build+restart in a from-genesis cycle).

Estimated true scope: **400–600 LOC** across `sync/message/snapshot_manifest_response.rs` (+ `_v5` mirror), `state/state_sync_manifest/snapshot_manifest_manager.rs`, `sync/protocol_version.rs`, plus client-side construction of `RelatedData` from wire data + tests covering the wrong-hash negative case.

### 5.10 What landed this session vs what remains

| Component | Status |
| --- | --- |
| Config + plumbing | ✅ |
| FSM gates (transition + catch_up_completed + seed-bypass + blame-block override) | ✅ |
| Peer-friendly `TemporarySkipped` for post-anchor headers | ✅ |
| Anchor-header pre-fetch in `CatchUpCheckpointPhase::start()` | ✅ |
| **Direct-persist bypass for anchor headers under `locked_for_catchup`** | ✅ (session 3) |
| Graceful (no-panic) failure in `validate_blame_states` when local headers missing | ✅ |
| `validate_blame_states` chain-walk loop accepting trusted-checkpoint mode | ❌ (next session) |
| `validate_epoch_receipts` ditto | ❌ |
| Wire V5 protocol carrying `RelatedData` payload | ❌ |
| End-to-end snapshot completion under trusted-checkpoint | ❌ |

### 5.4 Out of scope (separate tickets)

- Tuning `EPOCH_SYNC_BATCH_SIZE` / `REQUEST_START_WAITING_TIME` for the tail catch-up after snapshot.
- Investigating why observed ~8 epochs/sec is 20× below the theoretical ceiling (probably a peer-side rate limit + per-batch acknowledgement RTT).
- Pruning paritydb WAL on the fleet: m1 was carrying 102k log files (533 MB → 384 MB during compaction). Compaction looks to be lagging behind writes; worth a separate ticket on storage health.
- Filesystem-level `mdbx_copy`/snapshot tooling for the operator who actually wants to seed a node from another node's disk (the "Option B from yesterday's session" workflow).

## 6. References

- [chain-model.md](chain-model.md) — block structure, PoW bypass semantics.
- [checkpoint-snapshot-lifecycle.md](checkpoint-snapshot-lifecycle.md) — snapshot creation/serving on the fleet.
- [storage-architecture.md](storage-architecture.md) — MDBX + paritydb layout that the snapshot reproduces.
- `crates/mazzecore/core/src/sync/state/snapshot_chunk_sync.rs` — the existing snapshot-download state machine that this design hooks into.
- `crates/mazzecore/core/src/verification.rs:365` — `verify_pow()` and its bypass switches.
