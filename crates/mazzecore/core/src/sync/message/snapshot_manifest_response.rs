// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::{
    block_data_manager::BlockExecutionResult,
    message::{GetMaybeRequestId, Message, MessageProtocolVersionBound, MsgId},
    sync::{
        message::{msgid, Context, Handleable, SnapshotManifestRequest},
        state::storage::RangedManifest,
        Error, ErrorKind, SYNC_PROTO_V1, SYNC_PROTO_V3, SYNC_PROTO_V4,
    },
};
use mazze_types::H256;
use network::service::ProtocolVersion;
use primitives::{MerkleHash, StateRoot};
use rlp::Encodable;
use rlp_derive::{RlpDecodable, RlpEncodable};

const SNAPSHOT_MANIFEST_RESPONSE_PROTOCOL_VERSION: u8 = 1;

#[derive(RlpDecodable, RlpEncodable, Default)]
pub struct SnapshotManifestResponse {
    pub request_id: u64,
    pub manifest: RangedManifest,
    // We actually need state_root_blame_vec for two epochs: snapshot_epoch_id
    // and its next snapshot + 1 epoch; and the state_root of snapshot_epoch_id
    // and the state root of its next snapshot + 1 epoch to get
    // snapshot_merkle_root of snapshot_epoch_id. The
    // current implementation passes state_blame_vec for the entire range of
    // snapshot_epoch_id to its next snapshot's trusted blame block,
    // which should be improved.
    //
    // TODO: reduce the data to pass over network.
    pub state_root_vec: Vec<StateRoot>,
    pub receipt_blame_vec: Vec<H256>,
    pub bloom_blame_vec: Vec<H256>,
    pub block_receipts: Vec<BlockExecutionResult>,

    // Debug only field.
    // TODO: can be deleted later.
    pub snapshot_merkle_root: MerkleHash,
}

build_msg_impl! {
    SnapshotManifestResponse, msgid::GET_SNAPSHOT_MANIFEST_RESPONSE,
    "SnapshotManifestResponse", SYNC_PROTO_V1, SYNC_PROTO_V3
}

#[derive(RlpDecodable, RlpEncodable)]
pub struct SnapshotManifestResponseV4 {
    pub request_id: u64,
    pub protocol_version: u8,
    pub manifest: RangedManifest,
    pub state_root_vec: Vec<StateRoot>,
    pub receipt_blame_vec: Vec<H256>,
    pub bloom_blame_vec: Vec<H256>,
    pub block_receipts: Vec<BlockExecutionResult>,
    pub snapshot_merkle_root: MerkleHash,
}

impl Default for SnapshotManifestResponseV4 {
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
        }
    }
}

build_msg_impl! {
    SnapshotManifestResponseV4, msgid::GET_SNAPSHOT_MANIFEST_RESPONSE,
    "SnapshotManifestResponseV4", SYNC_PROTO_V4, SYNC_PROTO_V4
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

impl Handleable for SnapshotManifestResponseV4 {
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

        ctx.manager.state_sync.handle_snapshot_manifest_response(
            ctx,
            self.into_legacy(),
            &request,
        )?;

        Ok(())
    }
}

impl SnapshotManifestResponse {
    fn validate(
        &self, _: &Context, request: &SnapshotManifestRequest,
    ) -> Result<(), Error> {
        validate_snapshot_manifest_response_fields(
            request,
            &self.state_root_vec,
            &self.receipt_blame_vec,
            &self.bloom_blame_vec,
        )
    }
}

impl SnapshotManifestResponseV4 {
    pub fn from_legacy(response: SnapshotManifestResponse) -> Self {
        Self {
            request_id: response.request_id,
            protocol_version: SNAPSHOT_MANIFEST_RESPONSE_PROTOCOL_VERSION,
            manifest: response.manifest,
            state_root_vec: response.state_root_vec,
            receipt_blame_vec: response.receipt_blame_vec,
            bloom_blame_vec: response.bloom_blame_vec,
            block_receipts: response.block_receipts,
            snapshot_merkle_root: response.snapshot_merkle_root,
        }
    }

    fn into_legacy(self) -> SnapshotManifestResponse {
        SnapshotManifestResponse {
            request_id: self.request_id,
            manifest: self.manifest,
            state_root_vec: self.state_root_vec,
            receipt_blame_vec: self.receipt_blame_vec,
            bloom_blame_vec: self.bloom_blame_vec,
            block_receipts: self.block_receipts,
            snapshot_merkle_root: self.snapshot_merkle_root,
        }
    }

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

        validate_snapshot_manifest_response_fields(
            request,
            &self.state_root_vec,
            &self.receipt_blame_vec,
            &self.bloom_blame_vec,
        )
    }
}

fn validate_snapshot_manifest_response_fields(
    request: &SnapshotManifestRequest, state_root_vec: &Vec<StateRoot>,
    receipt_blame_vec: &Vec<H256>, bloom_blame_vec: &Vec<H256>,
) -> Result<(), Error> {
    if request.is_initial_request() && state_root_vec.is_empty() {
        debug!("Responded snapshot manifest has empty blame states");
        bail!(ErrorKind::InvalidSnapshotManifest(
            "state blame vector not found".into()
        ));
    }

    if state_root_vec.len() != receipt_blame_vec.len()
        || state_root_vec.len() != bloom_blame_vec.len()
    {
        debug!("Responded snapshot manifest has mismatch blame states/receipts/blooms");
        bail!(ErrorKind::InvalidSnapshotManifest(
            "blame vector length mismatch".into()
        ));
    }

    Ok(())
}
