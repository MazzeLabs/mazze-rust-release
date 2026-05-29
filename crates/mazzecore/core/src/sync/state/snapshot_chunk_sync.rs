// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::sync::{
    error::Error,
    message::{
        msgid, Context, SnapshotManifestRequest, SnapshotManifestResponse,
        StateSyncCandidateRequest,
    },
    state::{
        state_sync_candidate::state_sync_candidate_manager::StateSyncCandidateManager,
        state_sync_chunk::snapshot_chunk_manager::{
            SnapshotChunkConfig, SnapshotChunkManager,
        },
        state_sync_manifest::snapshot_manifest_manager::{
            RelatedData, SnapshotManifestConfig, SnapshotManifestManager,
        },
        storage::{Chunk, ChunkKey, SnapshotSyncCandidate},
    },
    synchronization_state::PeerFilter,
    SynchronizationProtocolHandler,
};
use mazze_parameters::consensus_internal::REWARD_EPOCH_COUNT;
use mazze_storage::Result as StorageResult;
use mazze_types::H256;
use metrics::{Counter, CounterUsize};
use network::{node_table::NodeId, NetworkContext};
use parking_lot::RwLock;
use primitives::EpochId;
use std::{
    collections::HashSet,
    fmt::{Debug, Formatter},
    sync::Arc,
    time::{Duration, Instant},
};

/// D.2 — How many snapshot-aligned epochs above `epoch_to_sync` to
/// propose as candidates. Each step is `snapshot_epoch_count` epochs
/// (cf. `BlockDataManager::get_snapshot_epoch_count`). The default 6
/// covers a full era (`era_epoch_count / snapshot_epoch_count = 10`)
/// minus the trailing-headroom needed for the trusted-blame check —
/// generous for fast chains while bounded against canvass spam.
const D2_FORWARD_CANDIDATE_WINDOW: usize = 6;

lazy_static! {
    /// Counter for snapshot-sync transitions into Status::Invalid.
    /// Each increment means the node fell back from snapshot-based sync to
    /// legacy body sync for this era. See docs/flow-audit.md G-CC-4.
    static ref SNAPSHOT_SYNC_INVALID_TRANSITIONS: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "snapshot_sync",
            "invalid_transitions",
        );

    /// D.2 — Bumped when snapshot-sync truly falls back to legacy body
    /// sync (= genesis replay if no checkpoint state is on disk).
    /// Separate counters per `reason` discriminator so operators can
    /// tell apart "no peer offered any candidate" (network problem)
    /// from "manifest exhausted" (chunk-download failure) from
    /// "state-root mismatch" (peer-side corruption / wrong-fork).
    /// Distinct from `invalid_transitions` because that counts every
    /// transition into Status::Invalid — including ones where we
    /// resume (B.2 path), whereas this counts only the terminal
    /// genesis-replay-bound fallback. Phase D.2 in
    /// `docs/checkpoint-snapshot-lifecycle.md`.
    static ref SNAPSHOT_SYNC_FALLBACK_MANIFEST_EXHAUSTED: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "snapshot_sync",
            "fallback_total.manifest_exhausted",
        );
    static ref SNAPSHOT_SYNC_FALLBACK_NO_PEER_OFFERED: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "snapshot_sync",
            "fallback_total.no_peer_offered_candidate",
        );
    static ref SNAPSHOT_SYNC_LAST_DITCH_CANVASSES: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "snapshot_sync",
            "last_ditch_canvasses_total",
        );
    static ref SNAPSHOT_SYNC_LAST_DITCH_RECOVERIES: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "snapshot_sync",
            "last_ditch_recoveries_total",
        );
}

/// D.2 — Enumerate snapshot-aligned epochs the local header chain knows
/// about, ordered newest-first, ready to be proposed as alternative
/// snapshot-sync candidates. The original target (`primary`) is always
/// the *first* candidate the caller pairs with these; we generate
/// forward-direction (= newer) alternatives.
///
/// **Why**: on a fast chain producing many blocks per second, peers
/// regularly prune past the era-genesis epoch by the time a fresh node
/// joins. The original `start_sync` proposed a single candidate
/// (`epoch_to_sync`); if no peer still has it, the node fell back to
/// genesis replay even though everyone holds a newer snapshot. We
/// avoid that by canvassing for the newest snapshot-aligned epoch a
/// quorum of peers supports.
///
/// **Trusted-blame headroom**: each proposed candidate must be at
/// least `snapshot_epoch_count` below the network tip so that the
/// trusted-blame verification has a stable blame block to anchor
/// against (cf. `ConsensusGraph::catch_up_completed`). Candidates that
/// don't pass `get_trusted_blame_block_for_snapshot` later are
/// filtered at activation time.
fn enumerate_forward_candidates(
    sync_handler: &SynchronizationProtocolHandler,
    primary_hash: &EpochId, primary_height: u64,
) -> Vec<SnapshotSyncCandidate> {
    let data_man = &sync_handler.graph.data_man;
    let snapshot_epoch_count =
        data_man.get_snapshot_epoch_count() as u64;
    if snapshot_epoch_count == 0 {
        return Vec::new();
    }
    let best_height = sync_handler.graph.consensus.best_epoch_number();

    let mut alternatives = Vec::new();
    let mut height = primary_height.saturating_add(snapshot_epoch_count);
    let mut steps_remaining = D2_FORWARD_CANDIDATE_WINDOW;
    while steps_remaining > 0 {
        // Trusted-blame headroom: the candidate must be at least one
        // snapshot cadence below the chain tip we've already seen so
        // we have a stable blame block to anchor against.
        if height + snapshot_epoch_count > best_height {
            break;
        }
        if let Ok(hash) = sync_handler
            .graph
            .consensus
            .get_hash_from_epoch_number(primitives::EpochNumber::Number(height))
        {
            // Skip vacuous identities (already covered by `primary`).
            if &hash != primary_hash {
                alternatives.push(SnapshotSyncCandidate::FullSync {
                    height,
                    snapshot_epoch_id: hash,
                });
            }
        }
        height = height.saturating_add(snapshot_epoch_count);
        steps_remaining -= 1;
    }

    // Newest first — `set_active_candidate` picks the first one with
    // peer support, and the newest snapshot means the least catch-up
    // work after sync completes.
    alternatives.sort_by(|a, b| b.get_height().cmp(&a.get_height()));
    alternatives
}

/// Build the snapshot-sync candidate list for one `start_sync` round.
///
/// **Trusted-checkpoint (fast-sync) mode** — when one or more anchors are
/// configured, propose those KNOWN anchors directly, newest-first. Their
/// hashes come from config/baked-in defaults, so NO local header chain is
/// needed (a fresh joiner has none). This is what lets a fast-sync joiner
/// recover when its newest configured anchor has been pruned fleet-wide:
/// it falls through to the next-newest configured era checkpoint that a
/// peer still serves. Replaces the header-chain `enumerate_forward_candidates`
/// walk, which returns nothing for a header-less joiner (§5.16).
///
/// **Normal mode** (a full node that fell behind, with a real header
/// chain) — keep the original behaviour: the primary `epoch_to_sync` plus
/// the header-derived forward alternatives (D.2).
fn enumerate_sync_candidates(
    sync_handler: &SynchronizationProtocolHandler, epoch_to_sync: &EpochId,
    primary_height: u64,
) -> Vec<SnapshotSyncCandidate> {
    let trusted =
        sync_handler.graph.consensus.trusted_checkpoint_candidates();
    if !trusted.is_empty() {
        // Already newest-first; hashes are known so no header chain needed.
        return trusted
            .into_iter()
            .map(|(height, snapshot_epoch_id)| {
                SnapshotSyncCandidate::FullSync {
                    height,
                    snapshot_epoch_id,
                }
            })
            .collect();
    }
    let mut candidates = vec![SnapshotSyncCandidate::FullSync {
        height: primary_height,
        snapshot_epoch_id: *epoch_to_sync,
    }];
    candidates.extend(enumerate_forward_candidates(
        sync_handler,
        epoch_to_sync,
        primary_height,
    ));
    candidates
}

/// D.2 — Loud, structured operator-facing log explaining why
/// snapshot-sync is about to genesis-replay. One line, parseable.
fn log_terminal_fallback(
    reason: &'static str, epoch_to_sync: &EpochId, peers_canvassed: usize,
    candidates_tried: usize,
) {
    error!(
        "SNAPSHOT-SYNC FALLBACK to legacy/genesis-replay: \
         reason={}  epoch_to_sync={:?}  peers_canvassed={}  \
         candidates_tried={}  recovery_estimate=\"hours to days\". \
         Operator action: confirm at least one peer in active_peers \
         holds a snapshot for an epoch within the last \
         era_epoch_count window; if peers have pruned past every \
         snapshot-aligned epoch you have headers for, the only \
         recovery is full genesis replay.",
        reason, epoch_to_sync, peers_canvassed, candidates_tried,
    );
}

#[derive(Copy, Clone, PartialEq)]
pub enum Status {
    Inactive,
    RequestingCandidates,
    StartCandidateSync,
    DownloadingManifest(Instant),
    DownloadingChunks(Instant),
    Completed,
    Invalid,
}

impl Default for Status {
    fn default() -> Self {
        Status::Inactive
    }
}

impl Debug for Status {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        let status = match self {
            Status::Inactive => "inactive".into(),
            Status::RequestingCandidates => "requesting candidates".into(),
            Status::StartCandidateSync => {
                "about to request a candidate state".into()
            }
            Status::DownloadingManifest(t) => {
                format!("downloading manifest ({:?})", t.elapsed())
            }
            Status::DownloadingChunks(t) => {
                format!("downloading chunks ({:?})", t.elapsed())
            }
            Status::Completed => "completed".into(),
            Status::Invalid => "invalid".into(),
        };

        write!(f, "{}", status)
    }
}

// TODO: Implement OneStepSync / IncSync as this is currently only implemented
// for FullSync.
struct Inner {
    status: Status,

    sync_candidate_manager: StateSyncCandidateManager,
    // Initialized after we receive a valid manifest.
    chunk_manager: Option<SnapshotChunkManager>,
    manifest_manager: Option<SnapshotManifestManager>,

    related_data: Option<RelatedData>,
    manifest_attempts: usize,
    /// D.2 — Last-ditch canvass already fired for the current sync
    /// attempt. Latched on first attempt so we don't loop the canvass
    /// forever; cleared by `Inner::new` / `SnapshotChunkSync::reset`
    /// on era rollover. Without this flag the manifest-exhaustion
    /// path would re-canvass on every `update_status` tick.
    last_ditch_canvass_done: bool,
}

impl Default for Inner {
    fn default() -> Self {
        Self::new()
    }
}

impl Inner {
    fn new() -> Self {
        Self {
            sync_candidate_manager: Default::default(),
            status: Status::Inactive,
            related_data: None,
            chunk_manager: None,
            manifest_manager: None,
            manifest_attempts: 0,
            last_ditch_canvass_done: false,
        }
    }

    pub fn start_sync_for_candidate(
        &mut self, sync_candidate: SnapshotSyncCandidate,
        active_peers: HashSet<NodeId>, trusted_blame_block: H256,
        io: &dyn NetworkContext, sync_handler: &SynchronizationProtocolHandler,
        manifest_config: SnapshotManifestConfig,
    ) {
        if let Some(chunk_manager) = &mut self.chunk_manager {
            if chunk_manager.snapshot_candidate == sync_candidate {
                // TODO If the chunk manager does not make progress for a long
                // time, we should also resync the manifest,
                // because the manifest might be valid but also
                // malicious. For example, the chunk size might be larger than
                // MaxPacketSize so no one can return us that chunk.

                // The new candidate is not changed, so we can resume our
                // previous sync status with new `active_peers`.
                self.status = Status::DownloadingChunks(Instant::now());
                chunk_manager.set_active_peers(active_peers);
                return;
            }
        }
        info!(
            "start to sync state, snapshot_to_sync = {:?}, trusted blame block = {:?}",
            sync_candidate, trusted_blame_block);
        let manifest_manager = SnapshotManifestManager::new_and_start(
            sync_candidate,
            trusted_blame_block,
            active_peers,
            manifest_config,
            io,
            sync_handler,
        );
        self.manifest_manager = Some(manifest_manager);
        self.status = Status::DownloadingManifest(Instant::now());
    }

    pub fn start_sync(
        &mut self, current_era_genesis: EpochId,
        candidates: Vec<SnapshotSyncCandidate>, io: &dyn NetworkContext,
        sync_handler: &SynchronizationProtocolHandler,
    ) {
        let peers = PeerFilter::new(msgid::STATE_SYNC_CANDIDATE_REQUEST)
            .select_all(&sync_handler.syn);
        if peers.is_empty() {
            return;
        }
        self.status = Status::RequestingCandidates;
        self.sync_candidate_manager.reset(
            current_era_genesis,
            candidates.clone(),
            peers.clone(),
        );
        self.request_candidates(io, sync_handler, candidates, peers);
    }

    /// request state candidates from all peers
    fn request_candidates(
        &self, io: &dyn NetworkContext,
        sync_handler: &SynchronizationProtocolHandler,
        candidates: Vec<SnapshotSyncCandidate>, peers: Vec<NodeId>,
    ) {
        let request = StateSyncCandidateRequest {
            request_id: 0,
            candidates,
        };
        for peer in peers {
            sync_handler.request_manager.request_with_delay(
                io,
                Box::new(request.clone()),
                Some(peer),
                None,
            );
        }
    }
}

impl Debug for Inner {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(
            f,
            "(status = {:?}, pending_peers: {}, manifest: {:?}, chunks: {:?})",
            self.status,
            self.sync_candidate_manager.pending_peers().len(),
            self.manifest_manager,
            self.chunk_manager,
        )
    }
}

pub struct SnapshotChunkSync {
    inner: Arc<RwLock<Inner>>,
    config: StateSyncConfiguration,
}

impl SnapshotChunkSync {
    pub fn new(config: StateSyncConfiguration) -> Self {
        SnapshotChunkSync {
            inner: Default::default(),
            config,
        }
    }

    pub fn reset(&self) {
        *self.inner.write() = Inner::new();
    }

    pub fn status(&self) -> Status {
        self.inner.read().status
    }

    /// Height of the snapshot whose chunks have been fully imported and
    /// verified, taken from `RelatedData.snapshot_info.height`. Returns
    /// `None` if no manifest has been processed yet OR the manifest
    /// path never populated `related_data` (legacy fallback). Used by
    /// `CatchUpCheckpointPhase` to re-anchor the consensus era genesis
    /// at the snapshot block after `restore_execution_state` runs.
    pub fn completed_snapshot_height(&self) -> Option<u64> {
        self.inner
            .read()
            .related_data
            .as_ref()
            .map(|r| r.snapshot_info.height)
    }

    /// Single-version manifest entry point. The response now always
    /// carries `pre_computed`; when the server populated a non-default
    /// pre_computed payload we route to the bypass that skips
    /// `validate_blame_states` + `validate_epoch_receipts` and trusts
    /// the chunk-merkle floor (server lies → chunk-verification fails
    /// loud, never silently corrupts state). When the server couldn't
    /// build a payload (empty SnapshotInfo), we fall through to the
    /// legacy validation walks. See docs/fast-sync-design.md §5.13 / §5.14.
    pub fn handle_snapshot_manifest_response(
        &self, ctx: &Context, response: SnapshotManifestResponse,
        request: &SnapshotManifestRequest,
    ) -> Result<(), Error> {
        let inner = &mut *self.inner.write();

        // status mismatch
        if !matches!(inner.status, Status::DownloadingManifest(_)) {
            info!("Snapshot manifest received, but mismatch with current status {:?}", inner.status);
            return Ok(());
        };

        let use_bypass = response.pre_computed.snapshot_info.height > 0;

        if let Some(manifest_manager) = &mut inner.manifest_manager {
            let pre_computed = response.pre_computed.clone();
            let r = if use_bypass {
                manifest_manager.handle_snapshot_manifest_response_v5(
                    ctx,
                    response,
                    pre_computed,
                    request,
                )?
            } else {
                manifest_manager
                    .handle_snapshot_manifest_response(ctx, response, request)?
            };
            if let Some(related_data) = r {
                // update status
                inner.status = Status::DownloadingChunks(Instant::now());
                inner.chunk_manager =
                    Some(SnapshotChunkManager::new_and_start(
                        ctx,
                        manifest_manager.snapshot_candidate.clone(),
                        related_data.snapshot_info.clone(),
                        related_data.parent_snapshot_info.clone(),
                        manifest_manager.chunk_boundaries.clone(),
                        manifest_manager.chunk_boundary_proofs.clone(),
                        manifest_manager.chunk_hashes.clone(),
                        manifest_manager.active_peers.clone(),
                        self.config.chunk_config(),
                        // This delta_root is the intermediate_delta_root of
                        // the new snapshot, and this field will be used to
                        // fill new state_root in
                        // get_state_trees_for_next_epoch
                        related_data
                            .true_state_root_by_blame_info
                            .state_root
                            .delta_root,
                    )?);
                inner.related_data = Some(related_data);
            }
            debug!("sync state progress: {:?}", *inner);
        } else {
            error!("manifest manager is None in status {:?}", inner.status);
        }
        if matches!(inner.status, Status::DownloadingChunks(_)) {
            inner.manifest_manager = None;
        }
        Ok(())
    }

    pub fn handle_snapshot_chunk_response(
        &self, ctx: &Context, chunk_key: ChunkKey, chunk: Chunk,
    ) -> StorageResult<()> {
        let mut inner = self.inner.write();

        if !matches!(inner.status, Status::DownloadingChunks(_)) {
            info!("Snapshot chunk {:?} received, but mismatch with current status {:?}",
                chunk_key, inner.status);
            return Ok(());
        }

        if let Some(chunk_manager) = &mut inner.chunk_manager {
            if chunk_manager.add_chunk(ctx, chunk_key, chunk)? {
                // Once the status becomes Completed, it will never be changed
                // to another status, and all the related fields
                // (snapshot_id, trust_blame_block, receipts, e.t.c.)
                // of Inner will not be modified, because we return early in
                // `update_status`
                // and `handle_snapshot_manifest_response`. Thus, we can rely on
                // the phase changing thread
                // to call `restore_execution_state` later safely.
                inner.status = Status::Completed;
            }
        } else {
            debug!(
                "Chunk {:?} received in status {:?}",
                chunk_key, inner.status
            );
        }
        info!("sync state progress: {:?}", *inner);
        Ok(())
    }

    pub fn restore_execution_state(
        &self, sync_handler: &SynchronizationProtocolHandler,
    ) {
        let inner = self.inner.read();
        let related_data = inner
            .related_data
            .as_ref()
            .expect("Set after receving manifest");
        let mut deferred_block_hash =
            related_data.snapshot_info.get_snapshot_epoch_id().clone();
        // Snapshot recovery gives us a fully verified state root at the sync
        // point. Earlier reward-window entries reuse that root so receipt and
        // bloom commitments remain available during recovery; later execution
        // rewrites authoritative commitments as the chain advances.
        for i in related_data.blame_vec_offset
            ..(related_data.blame_vec_offset + REWARD_EPOCH_COUNT as usize)
        {
            info!(
                "insert_epoch_execution_commitment for block hash {:?}",
                &deferred_block_hash
            );
            sync_handler
                .graph
                .data_man
                .insert_epoch_execution_commitment(
                    deferred_block_hash,
                    related_data.true_state_root_by_blame_info.clone(),
                    related_data.receipt_blame_vec[i],
                    related_data.bloom_blame_vec[i],
                );
            let block = sync_handler
                .graph
                .data_man
                .block_header_by_hash(&deferred_block_hash)
                .unwrap();
            deferred_block_hash = *block.parent_hash();
        }
        for (block_hash, epoch_hash, receipts) in &related_data.epoch_receipts {
            sync_handler.graph.data_man.insert_block_execution_result(
                *block_hash,
                *epoch_hash,
                receipts.clone(),
                true, /* persistent */
            );
        }
    }

    /// TODO Handling manifest requesting separately
    /// Return Some if a candidate is ready and we can start requesting
    /// manifests
    pub fn handle_snapshot_candidate_response(
        &self, peer: &NodeId,
        supported_candidates: &Vec<SnapshotSyncCandidate>,
        requested_candidates: &Vec<SnapshotSyncCandidate>,
    ) {
        self.inner.write().sync_candidate_manager.on_peer_response(
            peer,
            supported_candidates,
            requested_candidates,
        )
    }

    pub fn on_peer_disconnected(&self, peer: &NodeId) {
        let mut inner = self.inner.write();
        inner.sync_candidate_manager.on_peer_disconnected(peer);
        if let Some(manifest_manager) = &mut inner.manifest_manager {
            manifest_manager.on_peer_disconnected(peer);
        }
        if let Some(chunk_manager) = &mut inner.chunk_manager {
            chunk_manager.on_peer_disconnected(peer);
        }
    }

    /// Reset status if we cannot make progress based on current peers and
    /// candidates
    pub fn update_status(
        &self, current_era_genesis: EpochId, epoch_to_sync: EpochId,
        io: &dyn NetworkContext, sync_handler: &SynchronizationProtocolHandler,
    ) {
        let mut inner = self.inner.write();
        // D.2 — Era rollover refresh: a new sync target deserves a
        // fresh manifest-retry budget AND a fresh last-ditch canvass
        // opportunity. The first-ever sync attempt (era_genesis still
        // default) is excluded — `start_sync` will initialise the
        // candidate manager's era field.
        if inner.sync_candidate_manager.current_era_genesis
            != current_era_genesis
            && inner.sync_candidate_manager.current_era_genesis
                != EpochId::default()
        {
            debug!(
                "D.2: era rollover detected ({:?} -> {:?}); resetting \
                 manifest_attempts + last_ditch_canvass_done.",
                inner.sync_candidate_manager.current_era_genesis,
                current_era_genesis,
            );
            inner.manifest_attempts = 0;
            inner.last_ditch_canvass_done = false;
        }
        if inner.manifest_attempts
            >= self.config.max_downloading_manifest_attempts
        {
            // D.2 — Pre-Invalid last-ditch canvass. The era-genesis
            // target we were asked to sync may be pruned at every
            // peer, while NEWER snapshot-aligned epochs are still
            // available everywhere. Before settling for legacy body
            // sync (= genesis replay for a fresh joiner), enumerate
            // forward candidates from the local header chain and ask
            // ALL connected peers which they support. If anyone
            // responds, restart sync with the discovered candidate
            // list. The flag latches so we don't loop forever.
            if !inner.last_ditch_canvass_done {
                inner.last_ditch_canvass_done = true;
                let primary_height = sync_handler
                    .graph
                    .data_man
                    .block_height_by_hash(&epoch_to_sync)
                    .unwrap_or(0);
                let candidates = enumerate_sync_candidates(
                    sync_handler,
                    &epoch_to_sync,
                    primary_height,
                );
                let forward_count = candidates.len().saturating_sub(1);
                let peers = PeerFilter::new(
                    msgid::STATE_SYNC_CANDIDATE_REQUEST,
                )
                .select_all(&sync_handler.syn);
                let peer_count = peers.len();
                SNAPSHOT_SYNC_LAST_DITCH_CANVASSES.inc(1);
                error!(
                    "snapshot-sync: exhausted max manifest attempts ({}) \
                     for epoch_to_sync={:?}. Mounting last-ditch canvass: \
                     {} forward snapshot-aligned candidates × {} peers \
                     before falling back to genesis replay.",
                    self.config.max_downloading_manifest_attempts,
                    epoch_to_sync,
                    forward_count,
                    peer_count,
                );
                if peer_count > 0 && !candidates.is_empty() {
                    // Reset retry budget for the broader candidate set.
                    inner.manifest_attempts = 0;
                    inner.manifest_manager = None;
                    inner.chunk_manager = None;
                    inner.related_data = None;
                    inner.status = Status::Inactive;
                    inner.start_sync(
                        current_era_genesis,
                        candidates,
                        io,
                        sync_handler,
                    );
                    SNAPSHOT_SYNC_LAST_DITCH_RECOVERIES.inc(1);
                    return;
                }
                // No peers / no candidates → fall through to terminal
                // fallback with the no-peer reason.
                SNAPSHOT_SYNC_FALLBACK_NO_PEER_OFFERED.inc(1);
                SNAPSHOT_SYNC_INVALID_TRANSITIONS.inc(1);
                log_terminal_fallback(
                    "no_peer_offered_candidate",
                    &epoch_to_sync,
                    peer_count,
                    candidates.len(),
                );
                inner.status = Status::Invalid;
                return;
            }
            // Already canvassed once. Real terminal fallback.
            SNAPSHOT_SYNC_FALLBACK_MANIFEST_EXHAUSTED.inc(1);
            SNAPSHOT_SYNC_INVALID_TRANSITIONS.inc(1);
            log_terminal_fallback(
                "manifest_exhausted",
                &epoch_to_sync,
                /* peers_canvassed */ 0,
                /* candidates_tried */ self
                    .config
                    .max_downloading_manifest_attempts,
            );
            inner.status = Status::Invalid;
            return;
        }

        debug!("sync state status before updating: {:?}", *inner);
        self.check_timeout(
            &mut *inner,
            &Context {
                // node_id is not used here
                node_id: Default::default(),
                io,
                manager: sync_handler,
            },
        );

        if inner.status == Status::Invalid {
            debug!(
                "sync state status remains invalid for {:?}; waiting for phase fallback/reset",
                epoch_to_sync
            );
            return;
        }

        // If we moves into the next era, we should force state_sync to change
        // the candidates to states with in the new stable era. If the
        // era stays the same and a new snapshot becomes available, we
        // only change candidates if old candidates cannot to be synced,
        // so a state can be synced with one era time instead of only
        // one snapshot time
        if inner.sync_candidate_manager.current_era_genesis
            == current_era_genesis
        {
            match inner.status {
                Status::Completed => return,
                Status::RequestingCandidates => {
                    if inner.sync_candidate_manager.pending_peers().is_empty() {
                        inner.status = Status::StartCandidateSync;
                        inner.sync_candidate_manager.set_active_candidate();
                        if inner
                            .sync_candidate_manager
                            .get_active_candidate_and_peers()
                            .is_none()
                        {
                            // D.2 — Don't surrender yet. The
                            // last-ditch canvass branch above will
                            // get a chance via the manifest-attempts
                            // exhaustion path on the next tick. Mark
                            // Inactive so the loop reissues
                            // `start_sync` (now with multi-candidate
                            // enumeration on the rebound at the
                            // bottom of `update_status`). We only
                            // truly terminate via the fallback paths
                            // above, which emit `fallback_total{reason}`.
                            warn!(
                                "snapshot-sync: no peers support the current candidate set for {:?}. \
                                 Bouncing back through Status::Inactive so a refreshed multi-candidate \
                                 enumeration can pick newer snapshot-aligned epochs (D.2).",
                                epoch_to_sync
                            );
                            inner.status = Status::Inactive;
                            inner.manifest_manager = None;
                            inner.chunk_manager = None;
                            inner.related_data = None;
                        }
                    }
                }
                Status::DownloadingManifest(_) => {
                    if inner
                        .manifest_manager
                        .as_ref()
                        .expect("always set in DownloadingManifest")
                        .is_inactive()
                    {
                        // The current candidate fails, so try to choose the
                        // next one.
                        inner.status = Status::StartCandidateSync;
                        inner.sync_candidate_manager.set_active_candidate();
                    }
                }
                Status::DownloadingChunks(_) => {
                    if inner
                        .chunk_manager
                        .as_ref()
                        .expect("always set in DownloadingChunks")
                        .is_inactive()
                    {
                        // The current candidate fails, so try to choose the
                        // next one.
                        inner.status = Status::StartCandidateSync;
                        inner.sync_candidate_manager.set_active_candidate();
                    }
                }
                _ => {}
            }
            if inner.status != Status::Invalid
                && inner.sync_candidate_manager.is_inactive()
                && inner
                    .chunk_manager
                    .as_ref()
                    .map_or(true, |m| m.is_inactive())
                && inner
                    .manifest_manager
                    .as_ref()
                    .map_or(true, |m| m.is_inactive())
            {
                // We are requesting candidates and all `pending_peers` timeout,
                // or we are syncing states and all
                // `active_peers` for all candidates timeout.
                warn!("current sync candidate becomes inactive: {:?}", inner);
                inner.status = Status::Inactive;
                inner.manifest_manager = None;
                inner.chunk_manager = None;
                inner.related_data = None;
            }
            // We need to start/restart syncing states for a candidate.
            if inner.status == Status::StartCandidateSync {
                if let Some((sync_candidate, active_peers)) = inner
                    .sync_candidate_manager
                    .get_active_candidate_and_peers()
                {
                    match sync_handler
                        .graph
                        .consensus
                        .get_trusted_blame_block_for_snapshot(
                            sync_candidate.get_snapshot_epoch_id(),
                        ) {
                        Some(trusted_blame_block) => {
                            inner.start_sync_for_candidate(
                                sync_candidate,
                                active_peers,
                                trusted_blame_block,
                                io,
                                sync_handler,
                                self.config.manifest_config(),
                            );
                        }
                        None => {
                            error!("failed to start checkpoint sync, the trusted blame block is unavailable, epoch_to_sync={:?}", epoch_to_sync);
                        }
                    }
                } else {
                    inner.status = Status::Inactive;
                }
            }
        } else {
            inner.status = Status::Inactive;
        }

        if inner.status == Status::Inactive {
            // D.2 — Multi-candidate enumeration on every restart. The
            // primary candidate is the era-genesis target we were
            // asked to sync, but we also propose forward
            // snapshot-aligned epochs from the local header chain so
            // peers that have pruned past `epoch_to_sync` can still
            // offer us a newer snapshot. `set_active_candidate` walks
            // these newest-first, so a successful sync to a newer
            // epoch costs less catch-up afterward.
            // Fast-sync exception: under an operator-configured trusted
            // checkpoint we have NO local header for `epoch_to_sync` (it
            // IS the anchor we're syncing to). Fall back to the
            // operator-supplied height in that case; otherwise preserve
            // the "checkpoint must have header" invariant.
            let height = match sync_handler
                .graph
                .data_man
                .block_header_by_hash(&epoch_to_sync)
            {
                Some(h) => h.height(),
                None => sync_handler
                    .graph
                    .consensus
                    .trusted_checkpoint()
                    .filter(|(_, hash)| *hash == epoch_to_sync)
                    .map(|(h, _)| h)
                    .expect(
                        "Syncing checkpoint should have available header \
                         or trusted-checkpoint height matching epoch_to_sync",
                    ),
            };
            let candidates = enumerate_sync_candidates(
                sync_handler,
                &epoch_to_sync,
                height,
            );
            debug!(
                "snapshot-sync: proposing {} candidate(s) for era_genesis={:?} \
                 (trusted-checkpoint mode={})",
                candidates.len(),
                current_era_genesis,
                !sync_handler
                    .graph
                    .consensus
                    .trusted_checkpoint_candidates()
                    .is_empty(),
            );
            inner.start_sync(current_era_genesis, candidates, io, sync_handler)
        }
        debug!("sync state status after updating: {:?}", *inner);
    }

    fn check_timeout(&self, inner: &mut Inner, ctx: &Context) {
        inner
            .sync_candidate_manager
            .check_timeout(&self.config.candidate_request_timeout);
        if let Some(manifest_manager) = &mut inner.manifest_manager {
            manifest_manager.check_timeout(ctx);
        }
        if let Some(chunk_manager) = &mut inner.chunk_manager {
            if !chunk_manager.check_timeout(ctx) {
                debug!("reset status to Inactive and redownload manifest");
                inner.status = Status::Inactive;
                inner.chunk_manager = None;
                inner.manifest_attempts += 1;
            }
        }
    }
}

pub struct StateSyncConfiguration {
    pub max_downloading_chunks: usize,
    pub candidate_request_timeout: Duration,
    pub chunk_request_timeout: Duration,
    pub manifest_request_timeout: Duration,
    pub max_downloading_manifest_attempts: usize,
}

impl StateSyncConfiguration {
    fn chunk_config(&self) -> SnapshotChunkConfig {
        SnapshotChunkConfig {
            max_downloading_chunks: self.max_downloading_chunks,
            chunk_request_timeout: self.chunk_request_timeout,
        }
    }

    fn manifest_config(&self) -> SnapshotManifestConfig {
        SnapshotManifestConfig {
            manifest_request_timeout: self.manifest_request_timeout,
        }
    }
}
