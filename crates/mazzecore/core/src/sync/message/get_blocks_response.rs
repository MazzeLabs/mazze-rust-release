// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::{
    message::RequestId,
    sync::{
        message::{
            metrics::BLOCK_HANDLE_TIMER, Context, GetBlocks, GetCompactBlocks,
            Handleable,
        },
        synchronization_protocol_handler::RecoverPublicTask,
        Error, ErrorKind,
    },
};
use mazze_types::H256;
use metrics::MeterTimer;
use primitives::Block;
use rlp::{Decodable, DecoderError, Encodable, Rlp, RlpStream};
use rlp_derive::{RlpDecodable, RlpEncodable};
use std::collections::HashSet;

#[derive(Debug, PartialEq, Default, RlpDecodable, RlpEncodable)]
pub struct GetBlocksResponse {
    pub request_id: RequestId,
    pub blocks: Vec<Block>,
}

impl Handleable for GetBlocksResponse {
    fn handle(self, ctx: &Context) -> Result<(), Error> {
        let _timer = MeterTimer::time_func(BLOCK_HANDLE_TIMER.as_ref());

        debug!(
            "on_blocks_response, get block hashes {:?}",
            self.blocks
                .iter()
                .map(|b| b.block_header.hash())
                .collect::<Vec<H256>>()
        );

        // TODO Check block size in advance to avoid attacks causing OOM.
        if ctx.manager.is_block_queue_full() {
            debug!("recover_public_queue is full, discard GetBlocksResponse");
            return Ok(());
        }

        for block in &self.blocks {
            debug!("transaction received by block: ratio=1");
            debug!(
                "new block received: block_header={:?}, tx_count={}, block_size={}",
                block.block_header,
                block.transactions.len(),
                block.size(),
            );
        }

        let (requested_blocks, delay): (HashSet<H256>, _) =
            match ctx.match_request(self.request_id) {
                Ok(req) => {
                    let delay = req.delay;
                    let requested_blocks: HashSet<H256> = req
                        .downcast_ref::<GetBlocks>(
                            ctx.io,
                            &ctx.manager.request_manager,
                        )?
                        .hashes
                        .iter()
                        .cloned()
                        .collect();
                    (requested_blocks, delay)
                }
                Err(e) => match &e.0 {
                    ErrorKind::RequestNotFound => {
                        trace!(
                            "late GetBlocksResponse request_id={} peer={:?} blocks={}",
                            self.request_id,
                            ctx.node_id,
                            self.blocks.len()
                        );
                        (
                            self.blocks
                                .iter()
                                .map(|b| b.block_header.hash())
                                .collect(),
                            None,
                        )
                    }
                    _ => return Err(e),
                },
            };

        ctx.manager.recover_public_queue.dispatch(
            ctx.io,
            RecoverPublicTask::new(
                self.blocks,
                requested_blocks,
                ctx.node_id.clone(),
                false,
                delay,
            ),
        );

        Ok(())
    }
}

//////////////////////////////////////////////////////////////

#[derive(Debug, PartialEq, Default)]
pub struct GetBlocksWithPublicResponse {
    pub request_id: RequestId,
    pub blocks: Vec<Block>,
}

impl Handleable for GetBlocksWithPublicResponse {
    fn handle(self, ctx: &Context) -> Result<(), Error> {
        debug!(
            "on_blocks_with_public_response, get block hashes {:?}",
            self.blocks
                .iter()
                .map(|b| b.block_header.hash())
                .collect::<Vec<H256>>()
        );
        // Guard against unbounded queuing when public blocks mode is enabled.
        if ctx.manager.is_block_queue_full() {
            warn!("recover_public_queue is full, discard GetBlocksWithPublicResponse");
            return Ok(());
        }
        let (req_hashes, delay): (HashSet<H256>, _) =
            match ctx.match_request(self.request_id) {
                Ok(req) => {
                    let delay = req.delay;

                    // Avoid RequestMessage::downcast_ref probing here because it has
                    // resend side effects on mismatch.
                    let req_hashes: HashSet<H256> = if let Some(req) =
                        req.request.as_any().downcast_ref::<GetCompactBlocks>()
                    {
                        req.hashes.iter().cloned().collect()
                    } else if let Some(req) =
                        req.request.as_any().downcast_ref::<GetBlocks>()
                    {
                        req.hashes.iter().cloned().collect()
                    } else {
                        return Err(ErrorKind::UnexpectedResponse.into());
                    };

                    (req_hashes, delay)
                }
                Err(e) => match &e.0 {
                    ErrorKind::RequestNotFound => {
                        trace!(
                            "late GetBlocksWithPublicResponse request_id={} peer={:?} blocks={}",
                            self.request_id,
                            ctx.node_id,
                            self.blocks.len()
                        );
                        (
                            self.blocks
                                .iter()
                                .map(|b| b.block_header.hash())
                                .collect(),
                            None,
                        )
                    }
                    _ => return Err(e),
                },
            };

        ctx.manager.recover_public_queue.dispatch(
            ctx.io,
            RecoverPublicTask::new(
                self.blocks,
                req_hashes,
                ctx.node_id.clone(),
                false, /* compact */
                delay,
            ),
        );

        Ok(())
    }
}

impl Encodable for GetBlocksWithPublicResponse {
    fn rlp_append(&self, stream: &mut RlpStream) {
        stream
            .begin_list(2)
            .append(&self.request_id)
            .begin_list(self.blocks.len());

        for block in self.blocks.iter() {
            stream.append_raw(&block.encode_with_tx_public(), 1);
        }
    }
}

impl Decodable for GetBlocksWithPublicResponse {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        let request_id = rlp.val_at(0)?;
        let rlp_blocks = rlp.at(1)?;
        let mut blocks = Vec::new();

        for i in 0..rlp_blocks.item_count()? {
            let rlp_block = rlp_blocks.at(i)?;
            // A malformed block from a peer is a decoding error, not a panic.
            blocks.push(Block::decode_with_tx_public(&rlp_block)?);
        }

        Ok(GetBlocksWithPublicResponse { request_id, blocks })
    }
}

#[cfg(test)]
mod tests {
    use super::GetBlocksWithPublicResponse;
    use primitives::{shielded::ShieldedTxBody, Block, BlockHeaderBuilder};
    use rlp::RlpStream;
    use std::sync::Arc;

    fn block(shielded: u32) -> Block {
        let header = BlockHeaderBuilder::new().with_height(3).build();
        Block::new(header, vec![]).with_shielded(
            (0..shielded)
                .map(|i| Arc::new(ShieldedTxBody::sample_for_tests(i)))
                .collect(),
        )
    }

    #[test]
    fn blocks_with_public_keep_the_shielded_section() {
        let msg = GetBlocksWithPublicResponse {
            request_id: 9,
            blocks: vec![block(0), block(2)],
        };
        let decoded: GetBlocksWithPublicResponse =
            rlp::decode(&rlp::encode(&msg)).unwrap();
        assert_eq!(decoded.request_id, 9);
        assert!(decoded.blocks[0].shielded.is_empty());
        assert_eq!(decoded.blocks[1].shielded, msg.blocks[1].shielded);
    }

    #[test]
    fn a_malformed_block_is_an_error_not_a_panic() {
        let mut s = RlpStream::new_list(2);
        s.append(&1u64);
        s.begin_list(1);
        s.begin_list(5).append(&0u8).append(&0u8).append(&0u8).append(&0u8).append(&0u8);
        assert!(rlp::decode::<GetBlocksWithPublicResponse>(&s.out()).is_err());
    }
}
