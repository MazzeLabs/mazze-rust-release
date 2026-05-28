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
| **Chain-prefix prefetch (batched epoch-hash walk)** | ✅ (session 4) |
| **Broad direct-persist bypass for chain-prefix headers** | ✅ (session 4) |
| Graceful (no-panic) failure in `validate_blame_states` + `validate_epoch_receipts` when local consensus context missing | ✅ (session 4) |
| `validate_blame_states` chain-walk loop succeeds with operator-configured anchors | ✅ (session 4 — confirmed satisfied with `blame_height ≥ snapshot + 2053`) |
| `validate_epoch_receipts` accepts trusted-checkpoint mode | ❌ (consensus-engine dep — see §5.12) |
| Wire V5 protocol carrying `RelatedData` payload | ❌ (recommended path) |
| End-to-end snapshot completion under trusted-checkpoint | ❌ |

### 5.11 Session-4 findings — chain-prefix prefetch (option 1 from §5.6)

Implemented the batched chain-prefix walk in `CatchUpCheckpointPhase::next()`:

- On each FSM tick, walks blame_hash via parent_hash downward in `data_man`, finds the first missing parent, fires a `request_epoch_hashes_for_prefetch` for the next batch of 30 epoch numbers. The response handler chains automatically into `request_block_headers`, headers arrive, and a broadened bypass at the top of `insert_block_header` direct-persists them to `data_man` (skipping `locked_for_catchup`, skipping the orphan/not-ready dance, skipping consensus-graph insertion).
- Walks from `blame_height - 1` down to `snapshot_height - 2 * snapshot_epoch_count` (≈ 6760 headers for the live testnet's current chain depth), fully populating `data_man` for the heights `validate_blame_states` walks.
- Verified live: the parent-walk loop in `validate_blame_states` (which previously panicked at `:396`) now succeeds; the indexing math at `:555` also succeeds after the next session-4 fix below.

Surfaced a **new anchor constraint**: the protocol's response-indexing math at `snapshot_manifest_manager.rs:555` is `state_root_vec[offset - snapshot_blame_plus_depth]` where `snapshot_blame_plus_depth = snapshot_epoch_count = 2048`. So **`blame_height` must be ≥ `snapshot_height + snapshot_epoch_count + DEFERRED_STATE_EPOCH_COUNT` = `snapshot_height + 2053`**. The doc's operator-instruction "blame anchor a few hundred epochs above the snapshot" was wrong. Updated `hydra.local.toml` example accordingly.

### 5.12 The remaining hard blocker — consensus-engine dependency in `validate_epoch_receipts`

After the chain-prefix prefetch and the corrected anchor constraint, the snapshot-manifest validation **advances through both the parent-walk (`:396`) and the indexing math (`:555`)**, but panics at the next layer:

```text
crates/mazzecore/core/src/sync/state/state_sync_manifest/snapshot_manifest_manager.rs:604
  let ordered_executable_epoch_blocks = ctx.manager.graph.consensus
      .get_block_hashes_by_epoch(EpochNumber::Number(block_header.height()))
      .expect("ordered executable epoch blocks must exist");
```

`get_block_hashes_by_epoch` requires the **consensus engine** to have processed (and produced an executable-epoch ordering for) the queried height. Under trusted-checkpoint fast-sync we deliberately put headers into `data_man` *without* feeding them through `propagate_header_graph_status` and consensus execution — the whole point is to skip the cost. So this lookup returns `Err(EpochNotFound)`, and the `.expect` panics.

Session-4 patched this to a graceful `return None` (consistent with our earlier no-panic policy at `:316-321`), so the node no longer crashes — it instead drops into the existing `SNAPSHOT-SYNC FALLBACK → legacy genesis replay` path.

**Why patching this layer too doesn't close the gap:**

To make `validate_epoch_receipts` succeed under trusted-checkpoint, the operator would also have to supply (or the server would have to ship) the epoch-receipts and the `ordered_executable_epoch_blocks` mapping for `REWARD_EPOCH_COUNT (= 12)` epochs around the snapshot. Even then, the construction of `SnapshotInfo.main_chain_parts` (which is `Vec<EpochId>` of length `snapshot_epoch_count = 2048`) further requires `data_man.get_parent_epochs_for()` — another consensus-derived walk. Each layer assumes consensus has executed; client-side patches per-layer keep revealing the next.

### 5.13 Conclusion — option 1 is structurally bounded; option 2 (V5 wire) is the right path

This session **confirmed empirically** what the option 1 description warned about: even with the chain prefix and the direct-persist bypass, every downstream validation layer reaches back for consensus state that doesn't (and shouldn't) exist on a fresh trusted-checkpoint joiner. The locally-completable scope of option 1 ends at the consensus-engine dependency in `validate_epoch_receipts`.

**Option 2 stays the right architectural answer.** The server has all of: the snapshot's chain prefix, the receipts, the `SnapshotInfo`, the `StateRootWithAuxInfo`, and the `ordered_executable_epoch_blocks` for each epoch in the relevant window. It just doesn't ship them. A V5 wire bump carries that pre-computed `RelatedData` to the client; the client (under trusted-checkpoint) validates against `merkle_root` cryptographically against the chunks and bypasses every consensus-derived walk. This is the design where the operator's trusted hash IS the trust input and the wire response IS the verification surface.

The session-4 scaffolding is **not wasted work for option 2**: the config plumbing, the FSM-transition shortcuts, the seed-bypass, the direct-persist (for the anchor block headers — still needed so the merkle root binds the snapshot to its anchor), and the graceful no-panic policy are all reusable.

What option 2 still needs on top of what's landed:
- `SnapshotManifestResponseV5` field additions for `SnapshotInfo`, `parent_snapshot_info`, `StateRootWithAuxInfo`, and `Vec<Vec<H256>>` of `ordered_executable_epoch_blocks` for the REWARD_EPOCH_COUNT-window. (≈ 30 LOC.)
- Server-side: when responding to V5 requests, compute these fields (they're what `validate_blame_states` and `validate_epoch_receipts` would compute, so the logic is already there; just refactor it into a server-side producer and call it from the manifest handler). (≈ 100 LOC.)
- Client-side: when V5 received under trusted-checkpoint, populate `RelatedData` from the wire payload and skip both `validate_blame_states` and `validate_epoch_receipts` entirely (cryptographic verification of chunks against `merkle_root` remains as the safety floor). (≈ 50 LOC.)
- Version negotiation (`SYNC_PROTO_V5`) + wrong-hash negative test (chunk merkle mismatch → fail loud). (≈ 50 LOC + tests.)
- Fleet rollout via `/tmp/relaunch.sh` (well-proven by now).

Total: **≈ 230 LOC + tests**, all server- or client-side under `has_trusted_checkpoint()`, no consensus-affecting changes, no version-bump risk for non-fast-sync peers.

### 5.14 V5 wire bump — landed, verified end-to-end through chunk import

Commit `2e97af4` lands the V5 wire layer (~660 LOC). The 5-node Hetzner mesh + a local joiner with `trusted_checkpoint_height = 2048` exercised every step:

1. Local: `CatchUpCheckpointPhase` fires the `SnapshotManifestRequest` with the trusted-blame anchor.
2. Server (fleet): detects peer at `SYNC_PROTO_V5`, replies with `SnapshotManifestResponseV5` carrying `PreComputedRelatedData { snapshot_info, parent_snapshot_info, state_root_with_aux_info, blame_vec_offset, ordered_executable_epoch_blocks }`. Verified live: `merkle=0x28d56164…, height=2048, has_parent=false`.
3. Local: V5 manifest handler bypasses `validate_blame_states` + `validate_epoch_receipts`, leans on the chunk-merkle floor.
4. Local: downloads 3 chunks, `register_new_snapshot` fires (`SnapshotInfo { merkle_root: 0x28d56164…, parent_snapshot_height: 0, height: 2048 }`), phase transitions `CatchUpCheckpoint → CatchUpFillBlockBody → CatchUpSyncBlock`.

**Key dispatcher subtlety found and fixed in the same commit:** `handle_snapshot_manifest_response_message` originally tried V4 decoding first. V5 payload is a superset (V4 fields + extra `PreComputedRelatedData` field), and `Rlp` truncates silently — so V5 traffic was being decoded into V4 structs and routed through the slow validation path. Trying V5 before V4 in the dispatcher restored the bypass.

#### 5.14.1 Post-snapshot consensus-bootstrap fix

Commit `2c51d9a` patches the gap that immediately follows step 4. After `restore_execution_state` lands state + receipt commitments, `cur_consensus_era_genesis_hash` was still pointing at the true genesis. `CatchUpSyncBlockPhase` therefore set its sync horizon at height 0; peer-gossiped blocks from height ~19000 onward were correctly received as `NewBlockHashes` but rejected at `on_new_block` because they were unreachable from the (wrong) era origin. Symptom: `Catch-up mode: true, latest epoch: 0 missing_bodies: 0` indefinitely; `bestEpoch` never advanced.

Two-file fix:
- `SnapshotChunkSync::completed_snapshot_height()` — exposes `RelatedData.snapshot_info.height` (the merkle-verified bundle).
- `CatchUpCheckpointPhase::next()` at the `Status::Completed` branch — calls `set_cur_consensus_era_genesis_hash(snapshot_hash, snapshot_hash, height)` then `consensus.reset()` before transitioning. During fast-sync bootstrap the snapshot block plays both roles (era genesis = deferred-state origin AND era stable = checkpoint-monotonicity floor) until the chain advances past the next stable election. The C.3 monotonicity gate accepts this because `snapshot_height > 0 = previous_era_genesis_height`.

#### 5.14.2 Open follow-up — stale-anchor graceful fallback

End-to-end Normal-phase verification this session was blocked by an environmental issue: the fleet had been running ~1.5h since `trusted_checkpoint = (2048, 0xa2c486b4…)` was pinned, advancing to epoch ~19000. With `additional_maintained_snapshot_count = 2` + `keepsPreStableSnapshot`, only snapshots at heights `{18432, 16384, 14336, 12288}` are still served; snapshot at 2048 has been pruned from every fleet node. All five peers return `StateSyncCandidateResponse.supported_candidates: []`, the §5.6 anchor-locked loop never reaches `Status::Completed`, and the §5.14.1 fix's new code path doesn't fire.

Two viable next moves, **not yet implemented**:
- **Operator path**: relaunch / re-pin `trusted_checkpoint` to a currently-served snapshot height before each fast-sync run. Trivial but inflexible.
- **Graceful-fallback path**: when the configured anchor returns empty candidates from every peer for some grace window (~10–30 s), have the candidate loop fall back to peer-discovered candidates (the existing D.2 multi-candidate enumeration with no operator constraint). The operator's role then degrades to "I trust at least one of the peer-offered snapshots vs. a known-good hash list" — which keeps the integrity floor (chunk merkle verification) and is the natural way to handle stale-but-still-bootstrap-needed cases.

### 5.4 Out of scope (separate tickets)

- Tuning `EPOCH_SYNC_BATCH_SIZE` / `REQUEST_START_WAITING_TIME` for the tail catch-up after snapshot.
- Investigating why observed ~8 epochs/sec is 20× below the theoretical ceiling (probably a peer-side rate limit + per-batch acknowledgement RTT).
- Pruning paritydb WAL on the fleet: m1 was carrying 102k log files (533 MB → 384 MB during compaction). Compaction looks to be lagging behind writes; worth a separate ticket on storage health.
- Filesystem-level `mdbx_copy`/snapshot tooling for the operator who actually wants to seed a node from another node's disk (the "Option B from yesterday's session" workflow).

### 5.15 Fleet-stability findings — steady-state at 4 blocks/sec (session 6)

Landed this session: protocol consolidation to a single `SYNC_PROTO_V1`
(`4db5cc1`), the full-fast OOM + seed-miss-wedge fixes (`54c0724`), and a
clean fleet relaunch on the fixed binary that **collapsed a 3-way fork**
back to one chain (verified: identical block hashes at ep500/1000/1200
across m1/m2/m3/m6). Those are durable wins.

What surfaced as the remaining, **systemic** issues — none a single bug,
all worth dedicated tickets (do NOT chase on the live fleet ad-hoc):

1. **Block rate vs single-node throughput.** `TARGET_AVERAGE_BLOCK_GENERATION_PERIOD = 250000` µs
   ([parameters/src/lib.rs](../crates/mazzecore/parameters/src/lib.rs#L202))
   targets **0.25 s blocks (4/sec)**, and difficulty auto-adjusts to keep
   that (settles at `INITIAL_DIFFICULTY = 5` given fleet hashrate — *not*
   test-mode pinning; `mode="dev"` is off). Keeping 4/sec is a product
   decision; the cost is that a node must *execute + verify* ≥4 blocks/sec
   to stay synced. Catch-up itself is fast (~14 epoch/s, PoW bypassed in
   `catch_up_mode`), so raw execution is not the limit — the pressure is
   **steady-state Normal-phase verification** and sync completeness.

2. **RandomX verify path inefficiencies** ([pow/cache.rs](../crates/mazzecore/core/src/pow/cache.rs#L58)):
   (a) `update_context` takes a **write lock unconditionally** before
   checking if the seed changed — serializes every PoW op though the seed
   only changes per 2048-epoch boundary; should be a read-lock fast path.
   (b) RandomX runs in **light mode** (`RandomXContext::new(seed, false)`),
   ~10× slower per-hash than full-dataset. Both raise steady-state verify
   cost. (Profiling note: m6 the tip-producer has spare CPU; the network
   **event loop** is its top CPU consumer, not execution/verify.)

3. **Sync-protocol stall behind a graph hole.** A behind node (m1) froze
   at epoch 1329 — `CatchUpSyncBlock`, `missing_bodies: 0`,
   `check_not_ready_frontier` spinning, **idle CPU** (not throughput-bound).
   It was requesting epoch hashes at the **tip** (`[24465..24496]`) instead
   of filling the `1330+` gap, so consensus could never advance past the
   hole. Epoch-sync targeting + graph-hole recovery need hardening; with
   only **1 peer** connected it could not self-heal.

4. **Premature-Normal config (still live in fleet `hydra.toml`):**
   `dev_allow_phase_change_without_peer = true` + `min_phase_change_normal_peer_count = 0`
   let a node declare **Normal at a stale height** when it momentarily has
   no normal-phase peers ([synchronization_phases.rs:841](../crates/mazzecore/core/src/sync/synchronization_phases.rs#L841)).
   On a multi-node fleet this should be `false` / `>=2` so a node actually
   catches up to its peers before going Normal (and so it can't mine a
   stale branch). This compounded the apparent "lag/fork".

5. **Small-mesh peering fragility.** Each node holds only 2–4 peers;
   restarts + the operations above repeatedly dropped nodes to 1 peer,
   which is where stalls (#3) become unrecoverable. Heavy manual churn
   (relaunch, rolling miner swap, m5 wipe, m3 restart) degraded the mesh.

6. **Deploy tooling clobbers live state.** Ad-hoc deploys that rsync the
   working-tree `run/hydra.toml` overwrite a node's live `bootnodes` /
   `mining_author` with stale committed values (this broke the m5
   fast-sync join with stale bootnode IDs). Fix: ad-hoc deploy scripts
   must preserve/re-pin live `bootnodes`+`mining_author`, or exclude
   `run/hydra.toml` from their rsync.

7. **Blacklisting removed entirely (was: `AlreadyThrottled` → 7-day ban, a
   miner-prone footgun). FIXED.**
   `handle_error` ([synchronization_protocol_handler.rs:799](../crates/mazzecore/core/src/sync/synchronization_protocol_handler.rs#L799))
   maps `ErrorKind::AlreadyThrottled` to `UpdateNodeOperation::Remove`,
   which used to call `node_database::set_blacklisted` — a **7-day** ban
   persisted to `net_config/blacklisted_nodes.json`. A node has per-peer
   message throttling (token buckets); the **highest-message-rate peer is
   a miner** (broadcasting blocks at 4/sec + serving requests), so a busy
   honest miner could blow a peer's bucket, keep sending, trip
   `AlreadyThrottled`, and get **banned across the fleet for a week** →
   peers drop toward 1 → the #5 cascade. By contrast `InvalidBlock` is
   only a `Failure`/demote (line 759), so the seed-wedge was *not* the
   blacklister — throttling was. This was also the most coherent
   explanation for the **pre-relaunch fork**: nodes likely 7-day-banned
   each other during the wedge era → partitioned → forked → the relaunch's
   `net_config` wipe cleared the bans → fork collapsed (unprovable now —
   files were wiped).

   **Fix landed (this session):** blacklisting is **disabled by design**
   for a permissionless network. In [node_database.rs](../crates/network/src/node_database.rs),
   `set_blacklisted` now only soft-demotes via `note_failure` (NO ban, NO
   removal from the node table) and `evaluate_blacklisted` always returns
   `false`, so an honest peer — including a just-joined low-difficulty
   miner — can always (re)connect. The `blacklisted_nodes` table is left
   inert (never populated) for on-disk compatibility. Rationale: a
   coordinated set of nodes must not be able to permanently exclude an
   honest peer; the soft, self-healing reputation signal (`note_failure` →
   demotion) still throttles/deprioritizes a genuinely misbehaving peer
   without locking it out. The `Remove`/`Demotion` ops in `handle_error`
   are retained — they now degrade to demotion rather than a hard ban.

**Root-cause chain:** 0.25 s blocks → followers/restarted nodes must keep
up at 4/sec → on this hardware + with #2/#3/#4 they fall behind/stall →
thin mesh (#5) makes stalls unrecoverable → and #7 (throttle→7-day-ban,
miner-prone) plausibly turned partitions into the persistent **fork**
(banned peers couldn't reconnect for a week; the relaunch's `net_config`
wipe cleared the bans, which is why the fork collapsed). #7 is now fixed
(blacklisting removed). Before `54c0724` this manifested as
wedge/OOM/fork. The fixes made it *safe* (no crash/fork), but
**converging every node at the tip at 4/sec is the open systemic work**
(parallel/faster verify, sync-completeness + epoch-sync targeting, and
peer-count robustness + the premature-Normal config flip).

## 6. References

- [chain-model.md](chain-model.md) — block structure, PoW bypass semantics.
- [checkpoint-snapshot-lifecycle.md](checkpoint-snapshot-lifecycle.md) — snapshot creation/serving on the fleet.
- [storage-architecture.md](storage-architecture.md) — MDBX + paritydb layout that the snapshot reproduces.
- `crates/mazzecore/core/src/sync/state/snapshot_chunk_sync.rs` — the existing snapshot-download state machine that this design hooks into.
- `crates/mazzecore/core/src/verification.rs:365` — `verify_pow()` and its bypass switches.
