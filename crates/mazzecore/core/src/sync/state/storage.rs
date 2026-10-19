// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::sync::{Error, ErrorKind};
use fallible_iterator::FallibleIterator;
use keccak_hash::keccak;
use lazy_static::lazy_static;
use malloc_size_of_derive::MallocSizeOf as DeriveMallocSizeOf;
use mazze_storage::{
    rlp_key_value_len,
    storage_db::{
        key_value_db::KeyValueDbIterableTrait, snapshot_db::SnapshotDbTrait,
        OpenSnapshotMptTrait, SnapshotDbManagerTrait,
    },
    MptSlicer, StorageManager, TrieProof,
};
use mazze_types::H256;
use metrics::{Counter, CounterUsize};
use parking_lot::Mutex;
use primitives::{EpochId, MerkleHash};
use rlp::{Decodable, DecoderError, Encodable, Rlp, RlpStream};
use rlp_derive::{RlpDecodable, RlpEncodable};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

/// Cache key for a per-page chunk_hashes computation. `chunk_size` is
/// part of the key because changing the protocol-config chunk size
/// changes the boundary layout and therefore the per-chunk hashes.
type ChunkHashCacheKey = (EpochId, Option<Vec<u8>>, u64);

/// Soft cap on the LRU. ~256 × ~100 hashes × 32 bytes ≈ 800 KB worst case.
const CHUNK_HASH_CACHE_CAPACITY: usize = 256;

lazy_static! {
    /// Hot cache of per-page chunk_hashes. Hits short-circuit the
    /// expensive `Chunk::load(..., u64::MAX) + keccak` loop on the
    /// manifest-serving path.
    ///
    /// Approximate-LRU eviction: when we hit the cap we drop the
    /// oldest half. O(N) eviction is fine — the path runs at most
    /// ~10/sec/peer (`SnapshotManifestRequest` throttle).
    static ref CHUNK_HASH_CACHE: Mutex<
        HashMap<ChunkHashCacheKey, Arc<Vec<H256>>>,
    > = Mutex::new(HashMap::new());

    static ref CHUNK_HASH_CACHE_HITS: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "snapshot_sync",
            "manifest_chunk_hash_cache_hits",
        );
    static ref CHUNK_HASH_CACHE_MISSES: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "snapshot_sync",
            "manifest_chunk_hash_cache_misses",
        );

    /// Bumped on a sidecar hit (LRU cold but disk index warm — typical
    /// after a node restart).
    static ref CHUNK_HASH_DISK_HITS: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "snapshot_sync",
            "manifest_chunk_hash_disk_hits",
        );
    /// Bumped when a request missed both the LRU and the sidecar and
    /// had to recompute. The result is then written to both caches.
    static ref CHUNK_HASH_FRESH_COMPUTES: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "snapshot_sync",
            "manifest_chunk_hash_fresh_computes",
        );
    /// Disk-IO errors on the sidecar. Non-fatal — we fall through to
    /// fresh compute — but sustained increments mean the sidecar isn't
    /// persisting and the per-restart cache window is reopening.
    static ref CHUNK_HASH_DISK_IO_ERRORS: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group(
            "snapshot_sync",
            "manifest_chunk_hash_disk_io_errors",
        );
}

// ----------------------------------------------------------------
// Durable on-disk chunk-hash index (per-snapshot sidecar).
//
// Lives at `<snapshot_db_path>/_chunk_index.bin`. The leading
// underscore keeps it out of ParityDB's namespace; `destroy_snapshot`
// cleans up via `fs::remove_dir_all` on the snapshot directory.
//
// Format: RLP-encoded `PersistedChunkHashIndex { Vec<PersistedChunkHashPage> }`.
// Writes are atomic (temp-file + rename).
//
// `start_chunk_key` is stored as `Vec<u8>` + an `is_some` flag rather
// than `Option<Vec<u8>>` because RLP encoding of `Option` is not
// stable across rlp_derive's lifecycle.
// ----------------------------------------------------------------

#[derive(Default, Clone, Debug, RlpEncodable, RlpDecodable)]
struct PersistedChunkHashPage {
    chunk_size: u64,
    start_chunk_key_is_some: u8,
    start_chunk_key: Vec<u8>,
    hashes: Vec<H256>,
}

#[derive(Default, Clone, Debug, RlpEncodable, RlpDecodable)]
struct PersistedChunkHashIndex {
    pages: Vec<PersistedChunkHashPage>,
}

impl PersistedChunkHashIndex {
    fn find(
        &self, chunk_size: u64, start_chunk_key: &Option<Vec<u8>>,
    ) -> Option<&Vec<H256>> {
        let (is_some_marker, key_bytes): (u8, &[u8]) = match start_chunk_key {
            Some(k) => (1, k.as_slice()),
            None => (0, &[]),
        };
        self.pages
            .iter()
            .find(|p| {
                p.chunk_size == chunk_size
                    && p.start_chunk_key_is_some == is_some_marker
                    && p.start_chunk_key.as_slice() == key_bytes
            })
            .map(|p| &p.hashes)
    }

    fn upsert(
        &mut self, chunk_size: u64, start_chunk_key: &Option<Vec<u8>>,
        hashes: Vec<H256>,
    ) {
        let (is_some_marker, key_bytes): (u8, Vec<u8>) = match start_chunk_key {
            Some(k) => (1, k.clone()),
            None => (0, Vec::new()),
        };
        if let Some(p) = self.pages.iter_mut().find(|p| {
            p.chunk_size == chunk_size
                && p.start_chunk_key_is_some == is_some_marker
                && p.start_chunk_key == key_bytes
        }) {
            p.hashes = hashes;
            return;
        }
        self.pages.push(PersistedChunkHashPage {
            chunk_size,
            start_chunk_key_is_some: is_some_marker,
            start_chunk_key: key_bytes,
            hashes,
        });
    }
}

/// Returns the canonical sidecar path for an epoch's chunk-hash
/// index. Lives inside the snapshot directory so `destroy_snapshot`'s
/// `remove_dir_all` cleans it up automatically.
fn chunk_index_sidecar_path(
    storage_manager: &StorageManager, snapshot_epoch_id: &EpochId,
) -> PathBuf {
    let mut p = storage_manager
        .get_storage_manager()
        .get_snapshot_manager()
        .get_snapshot_db_manager()
        .get_snapshot_db_path(snapshot_epoch_id);
    p.push("_chunk_index.bin");
    p
}

/// Read the sidecar from disk. Missing file → empty index (treated as
/// cold). Decode errors → logged + treated as cold (we'll regenerate
/// on next compute). IO errors → counted via
/// `manifest_chunk_hash_disk_io_errors` and treated as cold.
fn load_persisted_chunk_index(path: &std::path::Path) -> PersistedChunkHashIndex {
    use std::fs;
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return PersistedChunkHashIndex::default();
        }
        Err(e) => {
            CHUNK_HASH_DISK_IO_ERRORS.inc(1);
            warn!(
                "chunk_hash sidecar read failed at {:?}: {} — treating as cold",
                path, e
            );
            return PersistedChunkHashIndex::default();
        }
    };
    match rlp::decode::<PersistedChunkHashIndex>(&bytes) {
        Ok(idx) => idx,
        Err(e) => {
            warn!(
                "chunk_hash sidecar decode failed at {:?}: {:?} — treating as cold",
                path, e
            );
            PersistedChunkHashIndex::default()
        }
    }
}

/// Atomic write: temp file + rename. A crash mid-write leaves either
/// the old index intact (rename hasn't happened) or the new one
/// committed — never a half-written file.
fn save_persisted_chunk_index(
    path: &std::path::Path, idx: &PersistedChunkHashIndex,
) {
    use std::fs;
    let parent = match path.parent() {
        Some(p) => p,
        None => {
            CHUNK_HASH_DISK_IO_ERRORS.inc(1);
            warn!("chunk_hash sidecar path has no parent: {:?}", path);
            return;
        }
    };
    if !parent.exists() {
        // Parent directory disappeared between us computing and
        // writing — snapshot was probably destroyed concurrently.
        // Skip silently; the LRU still has the entry for this process
        // lifetime.
        return;
    }
    let tmp = parent.join("_chunk_index.bin.tmp");
    let bytes = rlp::encode(idx);
    if let Err(e) = fs::write(&tmp, &bytes) {
        CHUNK_HASH_DISK_IO_ERRORS.inc(1);
        warn!("chunk_hash sidecar tmp write failed at {:?}: {}", tmp, e);
        return;
    }
    if let Err(e) = fs::rename(&tmp, path) {
        CHUNK_HASH_DISK_IO_ERRORS.inc(1);
        warn!(
            "chunk_hash sidecar rename failed {:?} -> {:?}: {}",
            tmp, path, e
        );
    }
}

/// Half-eviction when the LRU passes capacity. Each entry is an
/// immutable snapshot page for a given (snapshot, chunk_size) tuple,
/// so coarse "iterate and drop" eviction is sufficient.
fn chunk_hash_cache_evict_if_full(
    cache: &mut HashMap<ChunkHashCacheKey, Arc<Vec<H256>>>,
) {
    if cache.len() <= CHUNK_HASH_CACHE_CAPACITY {
        return;
    }
    let to_drop = cache.len() - CHUNK_HASH_CACHE_CAPACITY / 2;
    let keys_to_drop: Vec<ChunkHashCacheKey> =
        cache.keys().take(to_drop).cloned().collect();
    for k in keys_to_drop {
        cache.remove(&k);
    }
}

#[derive(
    Clone, Hash, Ord, PartialOrd, PartialEq, Eq, Debug, DeriveMallocSizeOf,
)]
pub enum SnapshotSyncCandidate {
    OneStepSync {
        height: u64,
        snapshot_epoch_id: EpochId,
    },
    FullSync {
        height: u64,
        snapshot_epoch_id: EpochId,
    },
    IncSync {
        height: u64,
        base_snapshot_epoch_id: EpochId,
        snapshot_epoch_id: EpochId,
    },
}

impl SnapshotSyncCandidate {
    fn to_type_id(&self) -> u8 {
        match self {
            SnapshotSyncCandidate::OneStepSync { .. } => 0,
            SnapshotSyncCandidate::FullSync { .. } => 1,
            SnapshotSyncCandidate::IncSync { .. } => 2,
        }
    }

    pub fn get_snapshot_epoch_id(&self) -> &EpochId {
        match self {
            SnapshotSyncCandidate::OneStepSync {
                snapshot_epoch_id, ..
            } => snapshot_epoch_id,
            SnapshotSyncCandidate::FullSync {
                snapshot_epoch_id, ..
            } => snapshot_epoch_id,
            SnapshotSyncCandidate::IncSync {
                snapshot_epoch_id, ..
            } => snapshot_epoch_id,
        }
    }

    pub fn get_height(&self) -> u64 {
        match self {
            SnapshotSyncCandidate::OneStepSync { height, .. } => *height,
            SnapshotSyncCandidate::FullSync { height, .. } => *height,
            SnapshotSyncCandidate::IncSync { height, .. } => *height,
        }
    }
}

impl Encodable for SnapshotSyncCandidate {
    fn rlp_append(&self, s: &mut RlpStream) {
        match &self {
            SnapshotSyncCandidate::OneStepSync {
                height,
                snapshot_epoch_id,
            } => {
                s.begin_list(3)
                    .append(&self.to_type_id())
                    .append(height)
                    .append(snapshot_epoch_id);
            }
            SnapshotSyncCandidate::FullSync {
                height,
                snapshot_epoch_id,
            } => {
                s.begin_list(3)
                    .append(&self.to_type_id())
                    .append(height)
                    .append(snapshot_epoch_id);
            }
            SnapshotSyncCandidate::IncSync {
                height,
                base_snapshot_epoch_id,
                snapshot_epoch_id,
            } => {
                s.begin_list(4)
                    .append(&self.to_type_id())
                    .append(height)
                    .append(base_snapshot_epoch_id)
                    .append(snapshot_epoch_id);
            }
        }
    }
}

impl Decodable for SnapshotSyncCandidate {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        let type_id: u8 = rlp.val_at(0)?;
        let parsed = match type_id {
            0 => SnapshotSyncCandidate::OneStepSync {
                height: rlp.val_at(1)?,
                snapshot_epoch_id: rlp.val_at(2)?,
            },
            1 => SnapshotSyncCandidate::FullSync {
                height: rlp.val_at(1)?,
                snapshot_epoch_id: rlp.val_at(2)?,
            },
            2 => SnapshotSyncCandidate::IncSync {
                height: rlp.val_at(1)?,
                base_snapshot_epoch_id: rlp.val_at(2)?,
                snapshot_epoch_id: rlp.val_at(3)?,
            },
            _ => {
                return Err(DecoderError::Custom(
                    "Unknown SnapshotSyncCandidate type id",
                ))
            }
        };
        debug_assert_eq!(parsed.to_type_id(), type_id);
        Ok(parsed)
    }
}

#[derive(
    Clone,
    RlpEncodable,
    RlpDecodable,
    Ord,
    PartialOrd,
    Eq,
    PartialEq,
    Debug,
    Hash,
    DeriveMallocSizeOf,
)]
pub struct ChunkKey {
    lower_bound_incl: Option<Vec<u8>>,
    pub upper_bound_excl: Option<Vec<u8>>,
}

#[derive(Default, Clone)]
pub struct RangedManifest {
    pub chunk_boundaries: Vec<Vec<u8>>,
    pub chunk_boundary_proofs: Vec<TrieProof>,
    pub next: Option<Vec<u8>>,
    /// D.1 — keccak256 of `rlp(Chunk { keys, values })` for each chunk
    /// this manifest page is authoritative over. Aligned so that, in
    /// aggregate (after all pages stitched), the i-th entry corresponds
    /// to the i-th `ChunkKey` produced by
    /// `RangedManifest::convert_boundaries_to_chunks(all_boundaries)`.
    ///
    /// Per-page semantics:
    /// - Intermediate page (`next.is_some()`):
    ///   `chunk_hashes.len() == chunk_boundaries.len()` — one hash per
    ///   bounded chunk emitted by this page. The trailing unbounded
    ///   chunk belongs to a subsequent page.
    /// - Final page (`next.is_none()`):
    ///   `chunk_hashes.len() == chunk_boundaries.len() + 1` — the extra
    ///   entry is the hash of the trailing `(last_boundary, None)`
    ///   chunk.
    ///
    /// **Wire-format compatibility**: the RLP serde is conditional —
    /// pages serve 3-field RLP when `chunk_hashes.is_empty()` (legacy
    /// peers and forward-compat) and 4-field RLP otherwise. Consumers
    /// detect via `Rlp::item_count`. See Phase D.1 in
    /// `docs/checkpoint-snapshot-lifecycle.md`.
    pub chunk_hashes: Vec<H256>,
}

impl Encodable for RangedManifest {
    fn rlp_append(&self, s: &mut RlpStream) {
        if self.chunk_hashes.is_empty() {
            s.begin_list(3)
                .append_list::<Vec<u8>, Vec<u8>>(&self.chunk_boundaries)
                .append_list(&self.chunk_boundary_proofs)
                .append(&self.next);
        } else {
            s.begin_list(4)
                .append_list::<Vec<u8>, Vec<u8>>(&self.chunk_boundaries)
                .append_list(&self.chunk_boundary_proofs)
                .append(&self.next)
                .append_list(&self.chunk_hashes);
        }
    }
}

impl Decodable for RangedManifest {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        let item_count = rlp.item_count()?;
        let chunk_hashes = if item_count >= 4 {
            rlp.list_at(3)?
        } else {
            Vec::new()
        };
        Ok(RangedManifest {
            chunk_boundaries: rlp.list_at(0)?,
            chunk_boundary_proofs: rlp.list_at(1)?,
            next: rlp.val_at(2)?,
            chunk_hashes,
        })
    }
}

impl RangedManifest {
    /// Validate the manifest with specified snapshot merkle root and the
    /// requested start chunk key. Basically, the retrieved chunks should
    /// not be empty, and the proofs of all chunk keys are valid.
    pub fn validate(&self, snapshot_root: &MerkleHash) -> Result<(), Error> {
        if self.chunk_boundaries.len() != self.chunk_boundary_proofs.len() {
            bail!(ErrorKind::InvalidSnapshotManifest(
                "chunk and proof number do not match".into(),
            ));
        }
        if self.next.is_some() && self.chunk_boundaries.is_empty() {
            bail!(ErrorKind::InvalidSnapshotManifest(
                "manifest continuation requires at least one chunk boundary"
                    .into(),
            ));
        }
        if let Some(next) = &self.next {
            if next != self.chunk_boundaries.last().unwrap() {
                bail!(ErrorKind::InvalidSnapshotManifest(
                    "next does not match last boundary".into(),
                ));
            }
        }

        // D.1 — if the producer advertised per-chunk content hashes,
        // their cardinality must match the chunks-this-page covers.
        // Intermediate page → 1 hash per emitted boundary; final page
        // → boundaries + 1 (the trailing unbounded chunk). An empty
        // `chunk_hashes` is the legacy-peer / forward-compat fallback
        // and is accepted (consumer will skip per-chunk verification).
        if !self.chunk_hashes.is_empty() {
            let expected = if self.next.is_some() {
                self.chunk_boundaries.len()
            } else {
                self.chunk_boundaries.len() + 1
            };
            if self.chunk_hashes.len() != expected {
                bail!(ErrorKind::InvalidSnapshotManifest(format!(
                    "chunk_hashes cardinality mismatch: got {}, expected {} \
                     (boundaries={}, has_next={})",
                    self.chunk_hashes.len(),
                    expected,
                    self.chunk_boundaries.len(),
                    self.next.is_some()
                )));
            }
        }

        // validate the trie proof for all chunks
        for (chunk_index, proof) in
            self.chunk_boundary_proofs.iter().enumerate()
        {
            if proof.get_merkle_root() != snapshot_root {
                warn!(
                    "Manifest merkle root should be {:?}, get {:?}",
                    snapshot_root,
                    proof.get_merkle_root()
                );
                bail!(ErrorKind::InvalidSnapshotManifest(
                    "invalid proof merkle root".into(),
                ));
            }
            if !proof.if_proves_key(&self.chunk_boundaries[chunk_index]).0 {
                bail!(ErrorKind::InvalidSnapshotManifest(
                    "invalid proof".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn convert_boundaries_to_chunks(
        chunk_boundaries: Vec<Vec<u8>>,
    ) -> Vec<ChunkKey> {
        let mut chunks = Vec::with_capacity(chunk_boundaries.len());
        let mut lower = None;
        for key in chunk_boundaries {
            chunks.push(ChunkKey {
                lower_bound_incl: lower,
                upper_bound_excl: Some(key.clone()),
            });
            lower = Some(key);
        }
        chunks.push(ChunkKey {
            lower_bound_incl: lower,
            upper_bound_excl: None,
        });
        chunks
    }

    pub fn load(
        snapshot_to_sync: &SnapshotSyncCandidate, start_key: Option<Vec<u8>>,
        storage_manager: &StorageManager, chunk_size: u64, max_chunks: usize,
    ) -> Result<Option<(RangedManifest, MerkleHash)>, Error> {
        let snapshot_epoch_id = match snapshot_to_sync {
            SnapshotSyncCandidate::FullSync {
                snapshot_epoch_id, ..
            } => snapshot_epoch_id,
            SnapshotSyncCandidate::IncSync { .. } => {
                unimplemented!();
            }
            SnapshotSyncCandidate::OneStepSync { .. } => {
                unimplemented!();
            }
        };
        debug!(
            "begin to load manifest, snapshot_epoch_id = {:?}, start_key = {:?}",
            snapshot_epoch_id, start_key
        );

        let snapshot_db_manager =
            storage_manager.get_storage_manager().get_snapshot_manager();

        let snapshot_db = match snapshot_db_manager.get_snapshot_by_epoch_id(
            snapshot_epoch_id,
            /* try_open = */ true,
            true,
        )? {
            Some(db) => db,
            None => {
                debug!(
                    "failed to load manifest, cannot find snapshot {:?}",
                    snapshot_epoch_id
                );
                return Ok(None);
            }
        };
        let mut snapshot_mpt = snapshot_db.open_snapshot_mpt_shared()?;
        let merkle_root = snapshot_mpt.merkle_root;
        let mut slicer = match start_key {
            Some(ref key) => MptSlicer::new_from_key(&mut snapshot_mpt, key)?,
            None => MptSlicer::new(&mut snapshot_mpt)?,
        };

        let mut manifest = RangedManifest::default();
        let mut has_next = true;

        for i in 0..max_chunks {
            trace!("cut chunks for manifest, loop = {}", i);
            slicer.advance(chunk_size)?;
            match slicer.get_range_end_key() {
                None => {
                    has_next = false;
                    break;
                }
                Some(key) => {
                    manifest.chunk_boundaries.push(key.to_vec());
                    manifest.chunk_boundary_proofs.push(slicer.to_proof());
                }
            }
        }

        if has_next {
            manifest.next = Some(
                manifest
                    .chunk_boundaries
                    .last()
                    .expect("boundaries not empty if has next")
                    .clone(),
            );
        }

        // Populate per-chunk content hashes. Each chunk gets
        // `keccak256(rlp(Chunk { keys, values }))` so the consumer can
        // fail-fast on receive instead of detecting corruption only at
        // MPT-root verification time.
        //
        // Per-page cardinality:
        // - Intermediate page (has_next): one hash per boundary.
        // - Final page: boundaries + 1 (trailing unbounded chunk).
        //
        // Cache: process-wide LRU keyed by (epoch, start_chunk_key,
        // chunk_size). First serve pays the load+hash cost; later
        // serves hit the cache.
        let chunk_count = if has_next {
            manifest.chunk_boundaries.len()
        } else {
            manifest.chunk_boundaries.len() + 1
        };
        let cache_key: ChunkHashCacheKey =
            (*snapshot_epoch_id, start_key.clone(), chunk_size);

        // Tier 1 — In-process LRU. Hottest path; ~microseconds.
        if let Some(cached) = CHUNK_HASH_CACHE.lock().get(&cache_key).cloned() {
            if cached.len() == chunk_count {
                CHUNK_HASH_CACHE_HITS.inc(1);
                manifest.chunk_hashes = (*cached).clone();
            }
        }

        // Tier 2 — Disk sidecar. Pays after a process restart cleared
        // the LRU, but the sidecar persists across restarts. ~milliseconds.
        let sidecar_path =
            chunk_index_sidecar_path(storage_manager, snapshot_epoch_id);
        if manifest.chunk_hashes.is_empty() && chunk_count > 0 {
            let on_disk = load_persisted_chunk_index(&sidecar_path);
            if let Some(hashes) = on_disk.find(chunk_size, &start_key) {
                if hashes.len() == chunk_count {
                    CHUNK_HASH_DISK_HITS.inc(1);
                    manifest.chunk_hashes = hashes.clone();
                    // Promote into the in-process LRU so the next
                    // serve doesn't pay the disk read.
                    let mut cache = CHUNK_HASH_CACHE.lock();
                    cache.insert(
                        cache_key.clone(),
                        Arc::new(manifest.chunk_hashes.clone()),
                    );
                    chunk_hash_cache_evict_if_full(&mut cache);
                }
            }
        }

        // Tier 3 — Fresh compute. Paid at most once per
        // (snapshot, page, chunk_size) tuple over the snapshot's
        // on-disk lifetime; repeat requests hit Tier 1 or Tier 2.
        if manifest.chunk_hashes.is_empty() && chunk_count > 0 {
            CHUNK_HASH_FRESH_COMPUTES.inc(1);
            CHUNK_HASH_CACHE_MISSES.inc(1);
            manifest.chunk_hashes = Vec::with_capacity(chunk_count);
            let mut prev_boundary: Option<Vec<u8>> = start_key.clone();
            for i in 0..chunk_count {
                let upper_bound = if i < manifest.chunk_boundaries.len() {
                    Some(manifest.chunk_boundaries[i].clone())
                } else {
                    None
                };
                let chunk_key = ChunkKey {
                    lower_bound_incl: prev_boundary.clone(),
                    upper_bound_excl: upper_bound.clone(),
                };
                // u64::MAX cap: we're the producer, this is our own
                // data, and the upstream `Chunk::load` will only fault
                // if a single chunk is somehow gigantic. Sliced
                // chunks are bounded by the slicer; pass-through
                // here.
                let chunk = match Chunk::load(
                    snapshot_epoch_id,
                    &chunk_key,
                    storage_manager,
                    u64::MAX,
                )? {
                    Some(c) => c,
                    None => bail!(ErrorKind::InvalidSnapshotManifest(
                        "chunk vanished between slicing and hashing".into()
                    )),
                };
                let hash = keccak(&rlp::encode(&chunk));
                manifest.chunk_hashes.push(hash);
                prev_boundary = upper_bound;
            }
            // Promote into Tier 1 (LRU).
            {
                let mut cache = CHUNK_HASH_CACHE.lock();
                cache.insert(
                    cache_key,
                    Arc::new(manifest.chunk_hashes.clone()),
                );
                chunk_hash_cache_evict_if_full(&mut cache);
            }
            // Promote into Tier 2 (disk sidecar). Read-modify-write
            // pattern preserves any other pages already cached for
            // this snapshot. Failures are non-fatal — the LRU still
            // serves this process; the next process restart just
            // pays the fresh-compute cost again.
            let mut idx = load_persisted_chunk_index(&sidecar_path);
            idx.upsert(
                chunk_size,
                &start_key,
                manifest.chunk_hashes.clone(),
            );
            save_persisted_chunk_index(&sidecar_path, &idx);
        }

        debug!(
            "succeed to load manifest, chunks = {}, next_chunk_key = {:?}, hashes = {}",
            manifest.chunk_boundaries.len(),
            manifest.next,
            manifest.chunk_hashes.len()
        );

        Ok(Some((manifest, merkle_root)))
    }
}

#[derive(Default)]
pub struct Chunk {
    pub keys: Vec<Vec<u8>>,
    pub values: Vec<Vec<u8>>,
}

impl Encodable for Chunk {
    fn rlp_append(&self, s: &mut RlpStream) {
        s.begin_list(2)
            .append_list::<Vec<u8>, Vec<u8>>(&self.keys)
            .append_list::<Vec<u8>, Vec<u8>>(&self.values);
    }
}

impl Decodable for Chunk {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        Ok(Chunk {
            keys: rlp.list_at(0)?,
            values: rlp.list_at(1)?,
        })
    }
}

impl Chunk {
    /// Validate the chunk with specified key.
    pub fn validate(&self, key: &ChunkKey) -> Result<(), Error> {
        // chunk should not be empty
        if self.keys.is_empty() {
            // Legacy peers use an empty chunk to report a temporarily
            // unavailable snapshot chunk. Protocol v4 peers signal this
            // explicitly before chunk validation reaches this path.
            return Err(ErrorKind::EmptySnapshotChunk.into());
        }
        if self.keys.len() != self.values.len() {
            return Err(ErrorKind::InvalidSnapshotChunk(
                "keys and values do not match".into(),
            )
            .into());
        }
        // the key of first item in chunk should match with the requested key
        if let Some(ref start_key) = key.lower_bound_incl {
            if start_key != &self.keys[0] {
                return Err(ErrorKind::InvalidSnapshotChunk(
                    "key mismatch".into(),
                )
                .into());
            }
        }

        Ok(())
    }

    pub fn load(
        snapshot_epoch_id: &H256, chunk_key: &ChunkKey,
        storage_manager: &StorageManager, max_chunk_size: u64,
    ) -> Result<Option<Chunk>, Error> {
        debug!(
            "begin to load chunk, snapshot_epoch_id = {:?}, key = {:?}",
            snapshot_epoch_id, chunk_key
        );

        let snapshot_db_manager =
            storage_manager.get_storage_manager().get_snapshot_manager();

        let snapshot_db = match snapshot_db_manager.get_snapshot_by_epoch_id(
            snapshot_epoch_id,
            /* try_open = */ true,
            false,
        )? {
            Some(db) => db,
            None => {
                debug!("failed to load chunk, cannot find snapshot by checkpoint {:?}",
                       snapshot_epoch_id);
                return Ok(None);
            }
        };

        let mut kv_iterator = snapshot_db.snapshot_kv_iterator()?.take();
        let lower_bound_incl =
            chunk_key.lower_bound_incl.clone().unwrap_or_default();
        let upper_bound_excl =
            chunk_key.upper_bound_excl.as_ref().map(|k| k.as_slice());
        let mut kvs = kv_iterator
            .iter_range(lower_bound_incl.as_slice(), upper_bound_excl)?
            .take();

        let mut keys = Vec::new();
        let mut values = Vec::new();
        let mut chunk_size = 0;
        while let Some((key, value)) = kvs.next()? {
            chunk_size += rlp_key_value_len(key.len() as u16, value.len());
            if chunk_size > max_chunk_size {
                let msg =
                    format!("Exceed max allowed chunk size {}", max_chunk_size);
                error!("{}", msg);
                return Err(ErrorKind::InvalidSnapshotChunk(msg).into());
            }

            keys.push(key);
            values.push(value.into());
        }

        debug!(
            "complete to load chunk, items = {}, chunk_key = {:?}",
            keys.len(),
            chunk_key
        );

        Ok(Some(Chunk { keys, values }))
    }
}

// todo add necessary unit tests when code is stable
