use crate::sync::{
    message::{msgid, Context, SnapshotChunkRequest},
    state::{
        state_sync_chunk::restore::Restorer,
        storage::{Chunk, ChunkKey, RangedManifest, SnapshotSyncCandidate},
    },
    synchronization_state::PeerFilter,
};
use keccak_hash::keccak;
use lazy_static::lazy_static;
use malloc_size_of_derive::MallocSizeOf as DeriveMallocSizeOf;
use mazze_storage::{
    storage_db::SnapshotInfo, FullSyncVerifier, Result as StorageResult,
    TrieProof,
};
use mazze_types::H256;
use metrics::{Counter, CounterUsize, Gauge, GaugeUsize};
use network::node_table::NodeId;
use primitives::MerkleHash;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt::{Debug, Formatter},
    sync::Arc,
    time::{Duration, Instant},
};

lazy_static! {
    /// D.1 — Bumped whenever a downloaded chunk's `keccak256(rlp(chunk))`
    /// does NOT match the hash the manifest committed to for this
    /// ChunkKey. Means either the peer is malicious or the chunk got
    /// corrupted in flight. The chunk is dropped and re-requested from
    /// a different peer.
    static ref SNAPSHOT_SYNC_CHUNK_HASH_MISMATCH_TOTAL: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "snapshot_sync",
            "chunk_hash_mismatch_total",
        );

    /// Phase E — current number of chunks not yet requested. Updated
    /// on every change to `pending_chunks`.
    static ref SNAPSHOT_SYNC_CHUNKS_PENDING: Arc<dyn Gauge<usize>> =
        GaugeUsize::register_with_group("snapshot_sync", "chunks_pending");

    /// Phase E — current number of chunks in flight (requested but
    /// not yet received). Updated alongside `downloading_chunks`.
    static ref SNAPSHOT_SYNC_CHUNKS_DOWNLOADING: Arc<dyn Gauge<usize>> =
        GaugeUsize::register_with_group("snapshot_sync", "chunks_downloading");

    /// Phase E — cumulative chunks accepted by the restorer (i.e.,
    /// passed both hash and append checks). Per-sync-cycle counter
    /// resets only on `SnapshotChunkSync::reset`.
    static ref SNAPSHOT_SYNC_CHUNKS_COMPLETED_TOTAL: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "snapshot_sync",
            "chunks_completed_total",
        );

    /// Phase E — cumulative chunks dropped before acceptance (hash
    /// mismatch, restorer rejection, or timeout). The hash-mismatch
    /// component is also exposed separately as
    /// `chunk_hash_mismatch_total`.
    static ref SNAPSHOT_SYNC_CHUNKS_FAILED_TOTAL: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "snapshot_sync",
            "chunks_failed_total",
        );
}

pub struct SnapshotChunkManager {
    pub snapshot_candidate: SnapshotSyncCandidate,
    snapshot_info: SnapshotInfo,
    parent_snapshot_info: Option<SnapshotInfo>,
    active_peers: HashSet<NodeId>,
    pending_chunks: VecDeque<ChunkKey>,
    downloading_chunks: HashMap<ChunkKey, DownloadingChunkStatus>,
    downloading_attempts: HashMap<ChunkKey, usize>,
    num_downloaded: usize,
    config: SnapshotChunkConfig,
    /// D.1 — Manifest-committed `keccak256(rlp(chunk))` per `ChunkKey`.
    /// Populated at construction from the aggregated manifest
    /// `chunk_hashes`. Empty when the producer didn't advertise hashes
    /// (legacy peer), in which case `add_chunk` skips the fail-fast
    /// path and falls back to the slower restoration-time Merkle root
    /// validation.
    expected_chunk_hashes: HashMap<ChunkKey, H256>,

    restorer: Restorer,
    intermediate_trie_root_merkle: MerkleHash,
}

impl SnapshotChunkManager {
    pub fn new_and_start(
        ctx: &Context, snapshot_candidate: SnapshotSyncCandidate,
        snapshot_info: SnapshotInfo,
        parent_snapshot_info: Option<SnapshotInfo>,
        chunk_boundaries: Vec<Vec<u8>>, chunk_boundary_proofs: Vec<TrieProof>,
        chunk_hashes: Vec<H256>, active_peers: HashSet<NodeId>,
        config: SnapshotChunkConfig,
        intermediate_trie_root_merkle: MerkleHash,
    ) -> StorageResult<Self> {
        let mut restorer = Restorer::new(
            *snapshot_candidate.get_snapshot_epoch_id(),
            snapshot_info.merkle_root,
        );

        let verifier = FullSyncVerifier::new(
            chunk_boundaries.len() + 1,
            chunk_boundaries.clone(),
            chunk_boundary_proofs.clone(),
            snapshot_info.merkle_root,
            ctx.manager
                .graph
                .data_man
                .storage_manager
                .get_storage_manager()
                .get_snapshot_manager()
                .get_snapshot_db_manager(),
            snapshot_info.get_snapshot_epoch_id(),
            snapshot_info.height,
        )?;

        restorer.initialize_verifier(verifier);
        let chunks =
            RangedManifest::convert_boundaries_to_chunks(chunk_boundaries);

        // D.1 — Build the expected-hash table when the manifest pages
        // committed to per-chunk hashes. If `chunk_hashes` is empty
        // (legacy producer) or the cardinality doesn't match, refuse
        // to populate the table — the consumer will then fall through
        // to the slower MPT-root-at-restoration safety net but still
        // logs the mismatch.
        let expected_chunk_hashes = if chunk_hashes.is_empty() {
            HashMap::new()
        } else if chunk_hashes.len() == chunks.len() {
            chunks
                .iter()
                .cloned()
                .zip(chunk_hashes.into_iter())
                .collect()
        } else {
            warn!(
                "D.1: manifest chunk_hashes cardinality {} != chunk count {}; \
                 disabling fail-fast per-chunk verification for this snapshot \
                 (Merkle-root check at restoration time still applies)",
                chunk_hashes.len(),
                chunks.len()
            );
            HashMap::new()
        };

        let mut chunk_manager = Self {
            snapshot_candidate,
            snapshot_info,
            parent_snapshot_info,
            active_peers,
            pending_chunks: chunks.into(),
            downloading_chunks: Default::default(),
            downloading_attempts: Default::default(),
            num_downloaded: 0,
            config,
            expected_chunk_hashes,
            restorer,
            intermediate_trie_root_merkle,
        };
        chunk_manager.publish_gauges();
        chunk_manager.request_chunks(ctx);
        Ok(chunk_manager)
    }

    /// Phase E — Push the current pending/downloading counts into the
    /// `snapshot_sync.chunks_*` gauges. Cheap to call; safe to call on
    /// every queue mutation.
    fn publish_gauges(&self) {
        SNAPSHOT_SYNC_CHUNKS_PENDING.update(self.pending_chunks.len());
        SNAPSHOT_SYNC_CHUNKS_DOWNLOADING
            .update(self.downloading_chunks.len());
    }

    /// Add a received chunk, and request new ones if needed.
    /// Return `Ok(true)` if all chunks have been received and the snapshot is
    /// reconstructed. Return `Ok(false)` if there are chunks missing.
    pub fn add_chunk(
        &mut self, ctx: &Context, chunk_key: ChunkKey, chunk: Chunk,
    ) -> StorageResult<bool> {
        // If a response is in `downloading_chunks`, we can process
        // it regardless of our current status, because we allow chunk requests
        // to be resumed if the snapshot to sync is unchanged.
        //
        // There are two possible reasons that a response is not in
        // `downloading_chunks`:
        // 1. received a out-of-date snapshot chunk, e.g. new era started.
        // 2. Chunks are received after timeout.
        if self.downloading_chunks.remove(&chunk_key).is_none() {
            info!("Snapshot chunk received, but not in downloading queue, progess is {:?}", self);
            return Ok(false);
        }

        // D.1 — Fail-fast: verify the chunk's keccak256 content hash
        // against the manifest-committed expected hash before passing
        // it to the restorer. A mismatch indicates either a malicious
        // peer (the manifest was authenticated upstream via the
        // trusted-blame state-root check) or transit-level corruption.
        // Either way: drop the chunk, mark the peer as a failure
        // source, and re-queue the ChunkKey for a different peer.
        if let Some(expected) = self.expected_chunk_hashes.get(&chunk_key) {
            let actual = keccak(&rlp::encode(&chunk));
            if &actual != expected {
                SNAPSHOT_SYNC_CHUNK_HASH_MISMATCH_TOTAL.inc(1);
                SNAPSHOT_SYNC_CHUNKS_FAILED_TOTAL.inc(1);
                warn!(
                    "D.1: snapshot chunk hash mismatch for {:?} from peer {:?}: \
                     expected={:?}, got={:?}. Dropping chunk + re-requesting.",
                    chunk_key, ctx.node_id, expected, actual
                );
                self.pending_chunks.push_back(chunk_key.clone());
                self.note_failure(&ctx.node_id);
                self.publish_gauges();
                self.request_chunks(ctx);
                return Ok(false);
            }
        }

        self.num_downloaded += 1;

        if !self.restorer.append(chunk_key.clone(), chunk) {
            // Phase E — restorer rejected the chunk (invalid keys /
            // proof). Counted as failed; requeue for another peer.
            SNAPSHOT_SYNC_CHUNKS_FAILED_TOTAL.inc(1);
            warn!("Receive invalid chunk during appending {:?}", chunk_key);
            self.pending_chunks.push_back(chunk_key);
            self.note_failure(&ctx.node_id)
        } else {
            // Phase E — accepted into the restorer.
            SNAPSHOT_SYNC_CHUNKS_COMPLETED_TOTAL.inc(1);
        }
        self.publish_gauges();

        // begin to restore if all chunks downloaded
        if self.downloading_chunks.is_empty() && self.pending_chunks.is_empty()
        {
            debug!("Snapshot chunks are all downloaded",);

            // start to restore and update status
            self.restorer.finalize_restoration(
                ctx.manager.graph.data_man.storage_manager.clone(),
                self.snapshot_info.clone(),
                self.parent_snapshot_info.clone(),
                self.intermediate_trie_root_merkle.clone(),
            )?;
            return Ok(true);
        }
        self.request_chunks(ctx);
        Ok(false)
    }

    fn request_chunk_from_peer(
        &mut self, ctx: &Context, peer: &NodeId,
    ) -> Option<ChunkKey> {
        let chunk_key = self.pending_chunks.pop_front()?;

        let replaced = self.downloading_chunks.insert(
            chunk_key.clone(),
            DownloadingChunkStatus {
                peer: *peer,
                start_time: Instant::now(),
            },
        );
        debug_assert!(replaced.is_none());
        self.publish_gauges();

        let request = SnapshotChunkRequest::new(
            self.snapshot_candidate.clone(),
            chunk_key.clone(),
        );

        ctx.manager.request_manager.request_with_delay(
            ctx.io,
            Box::new(request),
            Some(*peer),
            None,
        );

        Some(chunk_key)
    }

    /// Request multiple chunks from random peers.
    fn request_chunks(&mut self, ctx: &Context) {
        let chosen_peers = PeerFilter::new(msgid::GET_SNAPSHOT_CHUNK)
            .choose_from(&self.active_peers)
            .select_n(
                self.config.max_downloading_chunks
                    - self.downloading_chunks.len(),
                &ctx.manager.syn,
            );
        for peer in chosen_peers {
            if self.request_chunk_from_peer(ctx, &peer).is_none() {
                break;
            }
        }
    }

    /// Remove timeout chunks and request new chunks.
    pub fn check_timeout(&mut self, ctx: &Context) -> bool {
        let mut timeout_chunks = Vec::new();
        for (chunk_key, status) in &self.downloading_chunks {
            if status.start_time.elapsed() > self.config.chunk_request_timeout {
                self.active_peers.remove(&status.peer);
                timeout_chunks.push(chunk_key.clone());
            }
        }
        for timeout_key in timeout_chunks {
            self.downloading_attempts
                .entry(timeout_key.clone())
                .and_modify(|attempts| *attempts += 1)
                .or_insert(1);

            if *self.downloading_attempts.get(&timeout_key).unwrap()
                >= ctx.manager.protocol_config.max_downloading_chunk_attempts
            {
                error!("Exceeds max attemps to download {:?} ", timeout_key);
                self.publish_gauges();
                return false;
            }

            self.downloading_chunks.remove(&timeout_key);
            self.pending_chunks.push_back(timeout_key);
            // Phase E — each timeout is a failed delivery attempt.
            SNAPSHOT_SYNC_CHUNKS_FAILED_TOTAL.inc(1);
        }
        self.publish_gauges();
        self.request_chunks(ctx);
        true
    }

    pub fn is_inactive(&self) -> bool {
        self.active_peers.is_empty()
    }

    pub fn set_active_peers(&mut self, new_active_peers: HashSet<NodeId>) {
        self.active_peers = new_active_peers;
    }

    pub fn on_peer_disconnected(&mut self, peer: &NodeId) {
        self.active_peers.remove(peer);
    }

    fn note_failure(&mut self, node_id: &NodeId) {
        self.active_peers.remove(node_id);
    }
}

impl Debug for SnapshotChunkManager {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(
            f,
            "(download = {}/{}/{}, active_peers: {})",
            self.pending_chunks.len(),
            self.downloading_chunks.len(),
            self.num_downloaded,
            self.active_peers.len(),
        )
    }
}

#[derive(DeriveMallocSizeOf)]
struct DownloadingChunkStatus {
    peer: NodeId,
    start_time: Instant,
}

#[derive(DeriveMallocSizeOf)]
pub struct SnapshotChunkConfig {
    pub max_downloading_chunks: usize,
    pub chunk_request_timeout: Duration,
}
