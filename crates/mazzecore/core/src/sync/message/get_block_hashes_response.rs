// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Single-version pre-launch sync protocol — `GetBlockHashesResponse`
//! carries the canonical V4 shape (per-epoch grouping via
//! `Vec<EpochHashes>`). The legacy flat-hash V1–V3 shape has been
//! dropped along with the rest of the consolidation; commit history
//! preserves it.

use crate::{
    message::RequestId,
    sync::{
        message::{Context, GetBlockHashesByEpoch, Handleable},
        Error, ErrorKind,
    },
};
use mazze_types::H256;
use rlp_derive::{RlpDecodable, RlpEncodable};
use std::collections::HashSet;

#[derive(Debug, PartialEq, RlpEncodable, RlpDecodable)]
pub struct EpochHashes {
    pub epoch: u64,
    pub hashes: Vec<H256>,
}

#[derive(Debug, PartialEq, RlpEncodable, RlpDecodable)]
pub struct GetBlockHashesResponse {
    pub request_id: RequestId,
    pub epoch_hashes: Vec<EpochHashes>,
}

impl Handleable for GetBlockHashesResponse {
    fn handle(self, ctx: &Context) -> Result<(), Error> {
        debug!("on_block_hashes_response, msg={:?}", self);

        let req = ctx.match_request(self.request_id)?;
        let delay = req.delay;
        let epoch_req = req.downcast_ref::<GetBlockHashesByEpoch>(
            ctx.io,
            &ctx.manager.request_manager,
        )?;
        let requested_epochs =
            epoch_req.epochs.iter().cloned().collect::<HashSet<_>>();
        let mut received_epochs = HashSet::new();
        let mut hashes = Vec::new();

        for epoch_hashes in self.epoch_hashes {
            if !requested_epochs.contains(&epoch_hashes.epoch)
                || !received_epochs.insert(epoch_hashes.epoch)
            {
                bail!(ErrorKind::UnexpectedResponse);
            }
            hashes.extend(epoch_hashes.hashes);
        }

        if received_epochs.is_empty() {
            warn!(
                "Empty GetBlockHashesResponse from peer {:?} for epochs {:?}",
                ctx.node_id, epoch_req.epochs
            );
        }

        ctx.manager.request_manager.epochs_received(
            ctx.io,
            requested_epochs,
            received_epochs,
            delay,
        );

        let missing_headers = hashes
            .iter()
            .filter(|h| !ctx.manager.graph.contains_block_header(h))
            .cloned()
            .collect();

        ctx.manager.request_block_headers(
            ctx.io,
            Some(ctx.node_id),
            missing_headers,
            true, /* ignore_db */
        );
        ctx.manager.start_sync(ctx.io);

        Ok(())
    }
}
