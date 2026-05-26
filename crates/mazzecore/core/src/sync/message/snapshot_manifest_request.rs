// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::{
    block_data_manager::BlockExecutionResult,
    message::{
        GetMaybeRequestId, Message, MessageProtocolVersionBound, MsgId,
        RequestId, SetRequestId,
    },
    sync::{
        message::{
            msgid, Context, DynamicCapability, EpochBlockHashes, Handleable,
            KeyContainer, PreComputedRelatedData, SnapshotManifestResponse,
        },
        request_manager::{AsAny, Request},
        state::storage::{RangedManifest, SnapshotSyncCandidate},
        Error, ProtocolConfiguration, SYNC_PROTO_V1,
    },
};
use malloc_size_of_derive::MallocSizeOf as DeriveMallocSizeOf;
use mazze_parameters::{
    consensus::DEFERRED_STATE_EPOCH_COUNT,
    consensus_internal::REWARD_EPOCH_COUNT,
};
use mazze_types::H256;
use network::service::ProtocolVersion;
use primitives::{EpochNumber, StateRoot};
use rlp::Encodable;
use rlp_derive::{RlpDecodable, RlpEncodable};
use std::{any::Any, time::Duration};

#[derive(Debug, Clone, RlpDecodable, RlpEncodable, DeriveMallocSizeOf)]
pub struct SnapshotManifestRequest {
    pub request_id: u64,
    pub snapshot_to_sync: SnapshotSyncCandidate,
    pub start_chunk: Option<Vec<u8>>,
    pub trusted_blame_block: Option<H256>,
}

build_msg_with_request_id_impl! {
    SnapshotManifestRequest, msgid::GET_SNAPSHOT_MANIFEST,
    "SnapshotManifestRequest", SYNC_PROTO_V1, SYNC_PROTO_V1
}

impl Handleable for SnapshotManifestRequest {
    fn handle(self, ctx: &Context) -> Result<(), Error> {
        info!(
            "SnapshotManifestRequest from peer={:?}: is_initial={}",
            ctx.node_id,
            self.is_initial_request(),
        );
        // TODO Handle the case where we cannot serve the snapshot
        let snapshot_merkle_root;
        let manifest = match RangedManifest::load(
            &self.snapshot_to_sync,
            self.start_chunk.clone(),
            &ctx.manager.graph.data_man.storage_manager,
            ctx.manager.protocol_config.chunk_size_byte,
            ctx.manager.protocol_config.max_chunk_number_in_manifest,
        ) {
            Ok(Some((m, merkle_root))) => {
                snapshot_merkle_root = merkle_root;
                m
            }
            _ => {
                // Empty response — server can't serve this snapshot.
                // Client sees an all-zero merkle_root and fails loud at
                // chunk verification, never silently accepts state.
                let mut response = SnapshotManifestResponse::default();
                response.request_id = self.request_id;
                ctx.send_response(&response)?;
                return Ok(());
            }
        };

        // Build the legacy field block (state_root_vec / blame vecs /
        // block_receipts / merkle_root) used by both validation paths.
        let (state_root_vec, receipt_blame_vec, bloom_blame_vec) =
            if self.is_initial_request() {
                self.get_blame_states(ctx).unwrap_or_default()
            } else {
                Default::default()
            };
        let block_receipts = if self.is_initial_request() {
            self.get_block_receipts(ctx).unwrap_or_default()
        } else {
            Default::default()
        };
        let snapshot_merkle_root = if self.is_initial_request() {
            snapshot_merkle_root
        } else {
            Default::default()
        };

        // Always ship the pre-computed RelatedData on the initial
        // request — this is what lets a fresh trusted-checkpoint joiner
        // bypass the consensus-derived validation walks. On
        // range-continuation manifests it stays at Default (the client
        // doesn't re-validate on continuation).
        let pre_computed = if self.is_initial_request() {
            // Build a legacy-shape response on the stack so the V5
            // producer can read it the same way it did in the
            // multi-version era. The on-wire response below copies
            // these fields into the canonical struct directly.
            let legacy = LegacyResponseSnapshot {
                request_id: self.request_id,
                manifest: manifest.clone(),
                snapshot_merkle_root,
                state_root_vec: state_root_vec.clone(),
                receipt_blame_vec: receipt_blame_vec.clone(),
                bloom_blame_vec: bloom_blame_vec.clone(),
                block_receipts: block_receipts.clone(),
            };
            self.build_pre_computed_related_data(ctx, &legacy)
                .unwrap_or_else(|| {
                    debug!(
                        "V5 producer: could not build PreComputedRelatedData \
                         for snapshot {:?}; shipping default (client fails \
                         loud at chunk verification if anchor is wrong)",
                        self.snapshot_to_sync.get_snapshot_epoch_id(),
                    );
                    PreComputedRelatedData::default()
                })
        } else {
            PreComputedRelatedData::default()
        };

        let response = SnapshotManifestResponse {
            request_id: self.request_id,
            protocol_version: 1,
            manifest,
            state_root_vec,
            receipt_blame_vec,
            bloom_blame_vec,
            block_receipts,
            snapshot_merkle_root,
            pre_computed,
        };
        ctx.send_response(&response)
    }
}

/// Thin shim used only by `build_pre_computed_related_data` — that
/// helper was written against the legacy field set, so we hand it a
/// struct with the same shape rather than re-fitting it to the new
/// canonical struct (which also carries `pre_computed`).
pub(crate) struct LegacyResponseSnapshot {
    pub request_id: u64,
    pub manifest: RangedManifest,
    pub snapshot_merkle_root: primitives::MerkleHash,
    pub state_root_vec: Vec<StateRoot>,
    pub receipt_blame_vec: Vec<H256>,
    pub bloom_blame_vec: Vec<H256>,
    pub block_receipts: Vec<BlockExecutionResult>,
}

impl SnapshotManifestRequest {
    pub fn new(
        snapshot_sync_candidate: SnapshotSyncCandidate,
        trusted_blame_block: Option<H256>, start_chunk: Option<Vec<u8>>,
    ) -> Self {
        SnapshotManifestRequest {
            request_id: 0,
            snapshot_to_sync: snapshot_sync_candidate,
            start_chunk,
            trusted_blame_block,
        }
    }

    pub fn is_initial_request(&self) -> bool {
        self.trusted_blame_block.is_some()
    }

    /// V5 producer: assemble the `PreComputedRelatedData` payload from
    /// the server's local consensus + storage state. Returns `None`
    /// when any required field can't be looked up (in which case the
    /// caller ships an empty payload — a V5 client without
    /// trusted-checkpoint mode then falls back to V4 validation, and
    /// a V5 client WITH trusted-checkpoint fails loud on the empty
    /// merkle_root vs chunks check). See docs/fast-sync-design.md §5.13.
    fn build_pre_computed_related_data(
        &self, ctx: &Context, response: &LegacyResponseSnapshot,
    ) -> Option<PreComputedRelatedData> {
        let snapshot_epoch_id =
            *self.snapshot_to_sync.get_snapshot_epoch_id();
        let trusted_blame_hash = self.trusted_blame_block?;

        let data_man = &ctx.manager.graph.data_man;
        let storage_manager =
            data_man.storage_manager.get_storage_manager();

        // 1) SnapshotInfo for the requested snapshot.
        let snapshot_info =
            storage_manager.get_snapshot_info_at_epoch(&snapshot_epoch_id)?;

        // 2) Parent SnapshotInfo (length-0 vec means "no parent").
        let parent_snapshot_info_vec =
            if snapshot_info.parent_snapshot_epoch_id
                == primitives::NULL_EPOCH
            {
                Vec::new()
            } else {
                match storage_manager.get_snapshot_info_at_epoch(
                    &snapshot_info.parent_snapshot_epoch_id,
                ) {
                    Some(p) => vec![p],
                    None => Vec::new(),
                }
            };

        // 3) StateRootWithAuxInfo for the snapshot. The V4
        //    validate_blame_states derives this from state_root_vec[
        //    offset] + a walk; the server has it directly via the
        //    epoch-execution commitment for `trusted_blame_block`'s
        //    deferred chain. Same source the V4 validator ends up at.
        let trusted_blame_header =
            data_man.block_header_by_hash(&trusted_blame_hash)?;
        let snapshot_header =
            data_man.block_header_by_hash(&snapshot_epoch_id)?;
        let mut deferred = trusted_blame_hash;
        for _ in 0..DEFERRED_STATE_EPOCH_COUNT {
            let h = data_man.block_header_by_hash(&deferred)?;
            deferred = *h.parent_hash();
        }
        let commitment =
            data_man.get_epoch_execution_commitment_with_db(&deferred)?;
        let state_root_with_aux_info = commitment.state_root_with_aux_info;

        // 4) blame_vec_offset — same calc the V4 client does.
        let offset = trusted_blame_header
            .height()
            .checked_sub(
                snapshot_header.height() + DEFERRED_STATE_EPOCH_COUNT as u64,
            )?;
        // Sanity: keep ourselves bounded by what's actually in
        // state_root_vec so the client can index safely.
        if (offset as usize) >= response.state_root_vec.len() {
            return None;
        }

        // 5) ordered_executable_epoch_blocks for REWARD_EPOCH_COUNT
        //    epochs walking back from the snapshot. Replaces the
        //    fresh client's consensus.get_block_hashes_by_epoch call.
        let mut ordered_executable_epoch_blocks = Vec::new();
        let mut epoch_hash = snapshot_epoch_id;
        for _ in 0..REWARD_EPOCH_COUNT {
            let header = data_man.block_header_by_hash(&epoch_hash)?;
            let blocks = ctx
                .manager
                .graph
                .consensus
                .get_block_hashes_by_epoch(EpochNumber::Number(
                    header.height(),
                ))
                .ok()?;
            ordered_executable_epoch_blocks.push(EpochBlockHashes {
                hashes: blocks,
            });
            if header.height() == 0 {
                break;
            }
            epoch_hash = *header.parent_hash();
        }

        Some(PreComputedRelatedData {
            snapshot_info,
            parent_snapshot_info: parent_snapshot_info_vec,
            state_root_with_aux_info,
            blame_vec_offset: offset,
            ordered_executable_epoch_blocks,
        })
    }

    /// This function returns the receipts of REWARD_EPOCH_COUNT epochs
    /// backward from the epoch of *snapshot_to_sync*. It needs to
    /// return receipts of so many epochs to the request sender due to
    /// the following reason. Let the epoch of *snapshot_to_sync* be E(i).
    /// In the node of the request sender, to compute the state of E(i+1),
    /// it would require to compute and include the reward of
    /// E(i+1-REWARD_EPOCH_COUNT).
    fn get_block_receipts(
        &self, ctx: &Context,
    ) -> Option<Vec<BlockExecutionResult>> {
        let mut epoch_receipts = Vec::new();
        let mut epoch_hash =
            self.snapshot_to_sync.get_snapshot_epoch_id().clone();
        for i in 0..REWARD_EPOCH_COUNT {
            if let Some(block) =
                ctx.manager.graph.data_man.block_header_by_hash(&epoch_hash)
            {
                match ctx.manager.graph.consensus.get_block_hashes_by_epoch(
                    EpochNumber::Number(block.height()),
                ) {
                    Ok(ordered_executable_epoch_blocks) => {
                        if i == 0
                            && *ordered_executable_epoch_blocks.last().unwrap()
                                != epoch_hash
                        {
                            debug!(
                                "Snapshot epoch id mismatched for epoch {}",
                                block.height()
                            );
                            return None;
                        }
                        for hash in &ordered_executable_epoch_blocks {
                            match ctx
                                .manager
                                .graph
                                .data_man
                                .block_execution_result_by_hash_with_epoch(
                                    hash,
                                    &epoch_hash,
                                    false, /* update_main_assumption */
                                    false, /* update_cache */
                                ) {
                                Some(block_execution_result) => {
                                    epoch_receipts.push(block_execution_result);
                                }
                                None => {
                                    debug!("Cannot get execution result for hash={:?} epoch_hash={:?}",
                                        hash, epoch_hash
                                    );
                                    return None;
                                }
                            }
                        }
                    }
                    Err(_) => {
                        debug!(
                            "Cannot get block hashes for epoch {}",
                            block.height()
                        );
                        return None;
                    }
                }
                // We have reached original genesis
                if block.height() == 0 {
                    break;
                }
                epoch_hash = block.parent_hash().clone();
            } else {
                warn!(
                    "failed to find block={} in db, peer={}",
                    epoch_hash, ctx.node_id
                );
                return None;
            }
        }
        Some(epoch_receipts)
    }

    /// return an empty vec if some information not exist in db, caller may find
    /// another peer to send the request; otherwise return a state_blame_vec
    /// of the requested block
    fn get_blame_states(
        &self, ctx: &Context,
    ) -> Option<(Vec<StateRoot>, Vec<H256>, Vec<H256>)> {
        let trusted_block = ctx
            .manager
            .graph
            .data_man
            .block_header_by_hash(&self.trusted_blame_block?)?;
        let snapshot_epoch_block =
            ctx.manager.graph.data_man.block_header_by_hash(
                self.snapshot_to_sync.get_snapshot_epoch_id(),
            )?;
        if trusted_block.height() < snapshot_epoch_block.height() {
            warn!(
                "receive invalid snapshot manifest request from peer={}",
                ctx.node_id
            );
            return None;
        }
        let mut block_hash = trusted_block.hash();
        let mut trusted_block_height = trusted_block.height();
        let mut blame_count = trusted_block.blame();
        let mut deferred_block_hash = block_hash;
        for _ in 0..DEFERRED_STATE_EPOCH_COUNT {
            deferred_block_hash = *ctx
                .manager
                .graph
                .data_man
                .block_header_by_hash(&deferred_block_hash)
                .expect("All headers exist")
                .parent_hash();
        }

        let min_vec_len = if snapshot_epoch_block.height() == 0 {
            trusted_block.height()
                - DEFERRED_STATE_EPOCH_COUNT
                - snapshot_epoch_block.height()
                + 1
        } else {
            trusted_block.height()
                - DEFERRED_STATE_EPOCH_COUNT
                - snapshot_epoch_block.height()
                + REWARD_EPOCH_COUNT
        };
        let mut state_root_vec = Vec::with_capacity(min_vec_len as usize);
        let mut receipt_blame_vec = Vec::with_capacity(min_vec_len as usize);
        let mut bloom_blame_vec = Vec::with_capacity(min_vec_len as usize);

        // loop until we have enough length of `state_root_vec`
        loop {
            if let Some(block) =
                ctx.manager.graph.data_man.block_header_by_hash(&block_hash)
            {
                // We've jumped to another trusted block.
                if block.height() + blame_count as u64 + 1
                    == trusted_block_height
                {
                    trusted_block_height = block.height();
                    blame_count = block.blame()
                }
                if let Some(commitment) = ctx
                    .manager
                    .graph
                    .data_man
                    .get_epoch_execution_commitment_with_db(
                        &deferred_block_hash,
                    )
                {
                    state_root_vec.push(
                        commitment.state_root_with_aux_info.state_root.clone(),
                    );
                    receipt_blame_vec.push(commitment.receipts_root);
                    bloom_blame_vec.push(commitment.logs_bloom_hash);
                } else {
                    warn!(
                        "failed to find block={} in db, peer={}",
                        block_hash, ctx.node_id
                    );
                    return None;
                }
                // We've collected enough states.
                if block.height() + blame_count as u64 == trusted_block_height
                    && state_root_vec.len() >= min_vec_len as usize
                {
                    break;
                }
                block_hash = *block.parent_hash();
                deferred_block_hash = *ctx
                    .manager
                    .graph
                    .data_man
                    .block_header_by_hash(&deferred_block_hash)
                    .expect("All headers received")
                    .parent_hash();
            } else {
                warn!(
                    "failed to find block={} in db, peer={}",
                    block_hash, ctx.node_id
                );
                return None;
            }
        }

        Some((state_root_vec, receipt_blame_vec, bloom_blame_vec))
    }
}

impl AsAny for SnapshotManifestRequest {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl Request for SnapshotManifestRequest {
    fn timeout(&self, conf: &ProtocolConfiguration) -> Duration {
        conf.snapshot_manifest_request_timeout
    }

    fn on_removed(&self, _inflight_keys: &KeyContainer) {}

    fn with_inflight(&mut self, _inflight_keys: &KeyContainer) {}

    fn is_empty(&self) -> bool {
        false
    }

    fn resend(&self) -> Option<Box<dyn Request>> {
        None
    }

    fn required_capability(&self) -> Option<DynamicCapability> {
        None
    }
}
