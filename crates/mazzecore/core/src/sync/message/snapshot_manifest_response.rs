// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Single-version pre-launch sync protocol — `SnapshotManifestResponse`
//! carries the legacy V4 fields PLUS a pre-computed `RelatedData`
//! bundle so a fresh trusted-checkpoint joiner can bypass the
//! consensus-derived validation walks. Cryptographic safety floor is
//! still the chunk-merkle check against `pre_computed.snapshot_info
//! .merkle_root`. See docs/fast-sync-design.md §5.13 / §5.14.

use crate::{
    block_data_manager::BlockExecutionResult,
    message::{GetMaybeRequestId, Message, MessageProtocolVersionBound, MsgId},
    sync::{
        message::{msgid, Context, Handleable, SnapshotManifestRequest},
        state::storage::RangedManifest,
        Error, ErrorKind, SYNC_PROTO_V1,
    },
};
use mazze_internal_common::StateRootWithAuxInfo;
use mazze_storage::storage_db::SnapshotInfo;
use mazze_types::H256;
use network::service::ProtocolVersion;
use primitives::{MerkleHash, StateRoot};
use rlp::Encodable;
use rlp_derive::{RlpDecodable, RlpEncodable};

const SNAPSHOT_MANIFEST_RESPONSE_PROTOCOL_VERSION: u8 = 1;

/// One epoch's worth of executable block hashes — wrapper so the
/// outer payload can RLP-encode a `Vec<EpochBlockHashes>` (the
/// auto-derive doesn't directly support `Vec<Vec<H256>>`).
#[derive(Clone, Debug, Default, RlpEncodable, RlpDecodable)]
pub struct EpochBlockHashes {
    pub hashes: Vec<H256>,
}

/// Pre-computed `RelatedData` payload that the server attaches to
/// every snapshot-manifest response. The server can produce every
/// field from its local consensus + storage state by direct lookup;
/// the client receiving this under `has_trusted_checkpoint()`
/// populates its `RelatedData` from these fields and bypasses the
/// consensus-derived validation walks (`validate_blame_states` +
/// `validate_epoch_receipts`) that a fresh fast-sync joiner cannot
/// run locally. The snapshot's cryptographic integrity is still
/// verified — `snapshot_info.merkle_root` against the downloaded
/// chunks — so a server lying about this payload fails loudly at
/// chunk verification, never silently corrupts state.
#[derive(Clone, Debug, RlpEncodable, RlpDecodable)]
pub struct PreComputedRelatedData {
    /// The snapshot's own metadata (merkle_root, main_chain_parts, etc.)
    pub snapshot_info: SnapshotInfo,
    /// Parent snapshot's metadata, encoded as a length-0 or length-1
    /// vec instead of `Option<SnapshotInfo>` to keep the RLP shape
    /// trivial: empty vec ⇒ no parent (e.g. when the snapshot is at
    /// height 0 or below `snapshot_epoch_count`).
    pub parent_snapshot_info: Vec<SnapshotInfo>,
    /// `StateRootWithAuxInfo` for the snapshot's deferred-state block.
    pub state_root_with_aux_info: StateRootWithAuxInfo,
    /// Offset into the `state_root_vec` where the snapshot's state
    /// root lives. Derivable from heights, included explicitly so the
    /// wire is self-describing.
    pub blame_vec_offset: u64,
    /// Ordered executable block hashes for `REWARD_EPOCH_COUNT`
    /// epochs walking back from the snapshot. Replaces the local
    /// `consensus.get_block_hashes_by_epoch` call inside
    /// `validate_epoch_receipts`. Index 0 is the snapshot's own
    /// epoch; index N walks back N epochs via `parent_hash`.
    pub ordered_executable_epoch_blocks: Vec<EpochBlockHashes>,
}

impl Default for PreComputedRelatedData {
    fn default() -> Self {
        // `StateRootWithAuxInfo` has no Default; build a zero state via
        // the genesis helper. An empty payload is the server's "I
        // cannot serve this snapshot" signal — the client then sees an
        // all-zero `merkle_root` and fails loud at chunk verification,
        // never trusts this default for state.
        Self {
            snapshot_info: SnapshotInfo::default(),
            parent_snapshot_info: Vec::new(),
            state_root_with_aux_info: StateRootWithAuxInfo::genesis(
                &primitives::MERKLE_NULL_NODE,
            ),
            blame_vec_offset: 0,
            ordered_executable_epoch_blocks: Vec::new(),
        }
    }
}

/// The single canonical snapshot-manifest response. Carries everything
/// a V4 client needed PLUS the pre-computed `RelatedData` bundle for
/// the fast-sync bypass. Pre-launch consolidation removed the older
/// V1–V4 variants (commit history retains them).
#[derive(RlpDecodable, RlpEncodable)]
pub struct SnapshotManifestResponse {
    pub request_id: u64,
    pub protocol_version: u8,
    pub manifest: RangedManifest,
    pub state_root_vec: Vec<StateRoot>,
    pub receipt_blame_vec: Vec<H256>,
    pub bloom_blame_vec: Vec<H256>,
    pub block_receipts: Vec<BlockExecutionResult>,
    pub snapshot_merkle_root: MerkleHash,
    pub pre_computed: PreComputedRelatedData,
}

impl Default for SnapshotManifestResponse {
    fn default() -> Self {
        Self {
            request_id: 0,
            protocol_version: SNAPSHOT_MANIFEST_RESPONSE_PROTOCOL_VERSION,
            manifest: Default::default(),
            state_root_vec: Default::default(),
            receipt_blame_vec: Default::default(),
            bloom_blame_vec: Default::default(),
            block_receipts: Default::default(),
            snapshot_merkle_root: Default::default(),
            pre_computed: Default::default(),
        }
    }
}

build_msg_impl! {
    SnapshotManifestResponse, msgid::GET_SNAPSHOT_MANIFEST_RESPONSE,
    "SnapshotManifestResponse", SYNC_PROTO_V1, SYNC_PROTO_V1
}

impl Handleable for SnapshotManifestResponse {
    fn handle(self, ctx: &Context) -> Result<(), Error> {
        let message = ctx.match_request(self.request_id)?;

        let request = message.downcast_ref::<SnapshotManifestRequest>(
            ctx.io,
            &ctx.manager.request_manager,
        )?;

        if let Err(e) = self.validate(ctx, request) {
            ctx.manager
                .request_manager
                .resend_request_to_another_peer(ctx.io, &message);
            return Err(e);
        }

        ctx.manager
            .state_sync
            .handle_snapshot_manifest_response(ctx, self, &request)?;

        Ok(())
    }
}

impl SnapshotManifestResponse {
    fn validate(
        &self, _: &Context, request: &SnapshotManifestRequest,
    ) -> Result<(), Error> {
        if self.protocol_version != SNAPSHOT_MANIFEST_RESPONSE_PROTOCOL_VERSION
        {
            debug!(
                "Responded snapshot manifest has unexpected protocol version {}",
                self.protocol_version
            );
            bail!(ErrorKind::InvalidSnapshotManifest(format!(
                "unexpected snapshot manifest response version {}",
                self.protocol_version
            )));
        }
        if request.is_initial_request() && self.state_root_vec.is_empty() {
            debug!("Responded snapshot manifest has empty blame states");
            bail!(ErrorKind::InvalidSnapshotManifest(
                "state blame vector not found".into()
            ));
        }
        if self.state_root_vec.len() != self.receipt_blame_vec.len()
            || self.state_root_vec.len() != self.bloom_blame_vec.len()
        {
            debug!("Responded snapshot manifest has mismatch blame states/receipts/blooms");
            bail!(ErrorKind::InvalidSnapshotManifest(
                "blame vector length mismatch".into()
            ));
        }
        Ok(())
    }
}
