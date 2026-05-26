// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Single-version pre-launch sync protocol — `SnapshotChunkResponse`
//! carries the canonical V4 shape with an explicit `available` flag
//! so the server can answer "I don't have this chunk" without sending
//! a sentinel-empty payload that overlapping with valid empty chunks.
//! The legacy V1–V3 shape has been dropped; commit history preserves it.

use crate::{
    message::{GetMaybeRequestId, Message, MessageProtocolVersionBound, MsgId},
    sync::{
        message::{msgid, Context, Handleable, SnapshotChunkRequest},
        state::storage::Chunk,
        Error, ErrorKind, SYNC_PROTO_V1,
    },
};
use network::service::ProtocolVersion;
use rlp::Encodable;
use rlp_derive::{RlpDecodable, RlpEncodable};

#[derive(RlpDecodable, RlpEncodable)]
pub struct SnapshotChunkResponse {
    pub request_id: u64,
    pub available: bool,
    pub chunk: Chunk,
}

impl SnapshotChunkResponse {
    pub fn available(request_id: u64, chunk: Chunk) -> Self {
        Self {
            request_id,
            available: true,
            chunk,
        }
    }

    pub fn unavailable(request_id: u64) -> Self {
        Self {
            request_id,
            available: false,
            chunk: Chunk::default(),
        }
    }
}

build_msg_impl! {
    SnapshotChunkResponse, msgid::GET_SNAPSHOT_CHUNK_RESPONSE,
    "SnapshotChunkResponse", SYNC_PROTO_V1, SYNC_PROTO_V1
}

impl Handleable for SnapshotChunkResponse {
    fn handle(self, ctx: &Context) -> Result<(), Error> {
        let message = ctx.match_request(self.request_id)?;

        let request = message.downcast_ref::<SnapshotChunkRequest>(
            ctx.io,
            &ctx.manager.request_manager,
        )?;

        debug!(
            "handle_snapshot_chunk_response key={:?} available={} chunk_len={}",
            request.chunk_key,
            self.available,
            self.chunk.keys.len()
        );

        if !self.available {
            if !self.chunk.keys.is_empty() || !self.chunk.values.is_empty() {
                bail!(ErrorKind::InvalidSnapshotChunk(
                    "unavailable chunk response must not carry payload".into(),
                ));
            }
            ctx.manager
                .request_manager
                .resend_request_to_another_peer(ctx.io, &message);
            bail!(ErrorKind::EmptySnapshotChunk);
        }

        if let Err(e) = self.chunk.validate(&request.chunk_key) {
            debug!("failed to validate the snapshot chunk, error = {}", e);
            ctx.manager
                .request_manager
                .resend_request_to_another_peer(ctx.io, &message);
            return Err(e);
        }

        ctx.manager.state_sync.handle_snapshot_chunk_response(
            ctx,
            request.chunk_key.clone(),
            self.chunk,
        )?;

        Ok(())
    }
}
