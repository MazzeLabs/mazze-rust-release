// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

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
pub struct GetBlockHashesResponse {
    pub request_id: RequestId,
    pub hashes: Vec<H256>,
}

#[derive(Debug, PartialEq, RlpEncodable, RlpDecodable)]
pub struct EpochHashes {
    pub epoch: u64,
    pub hashes: Vec<H256>,
}

#[derive(Debug, PartialEq, RlpEncodable, RlpDecodable)]
pub struct GetBlockHashesResponseV4 {
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

        if self.hashes.is_empty() {
            warn!(
                "Empty GetBlockHashesResponse from peer {:?} for epochs {:?}",
                ctx.node_id, epoch_req.epochs
            );
            ctx.manager.request_manager.epochs_received(
                ctx.io,
                epoch_req.epochs.iter().cloned().collect(),
                HashSet::new(),
                delay,
            );
            return Ok(());
        }

        // Legacy responses flatten hashes across epochs, so the receiver
        // treats any non-empty payload as progress for all requested epochs and
        // lets dependent header requests recover the exact branch.
        let req = epoch_req.epochs.iter().cloned().collect();
        let rec = epoch_req.epochs.iter().cloned().collect();
        ctx.manager
            .request_manager
            .epochs_received(ctx.io, req, rec, delay);

        // request missing headers
        let missing_headers = self
            .hashes
            .iter()
            .filter(|h| !ctx.manager.graph.contains_block_header(&h))
            .cloned()
            .collect();

        // NOTE: this is to make sure no section of the DAG is skipped
        // e.g. if the request for epoch 4 is lost or the reply is in-
        // correct, the request for epoch 5 should recursively request
        // all dependent blocks (see on_block_headers_response)

        ctx.manager.request_block_headers(
            ctx.io,
            Some(ctx.node_id),
            missing_headers,
            true, /* ignore_db */
        );
        // try requesting some more epochs
        ctx.manager.start_sync(ctx.io);

        Ok(())
    }
}

impl Handleable for GetBlockHashesResponseV4 {
    fn handle(self, ctx: &Context) -> Result<(), Error> {
        debug!("on_block_hashes_response_v4, msg={:?}", self);

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
                "Empty GetBlockHashesResponseV4 from peer {:?} for epochs {:?}",
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
