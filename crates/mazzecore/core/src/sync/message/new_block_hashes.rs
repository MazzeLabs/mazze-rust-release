// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::sync::{
    message::{Context, Handleable},
    Error,
};
use mazze_types::H256;
use rlp::{Decodable, DecoderError, Encodable, Rlp, RlpStream};

#[derive(Debug, PartialEq)]
pub struct NewBlockHashes {
    pub block_hashes: Vec<H256>,
}

impl Encodable for NewBlockHashes {
    fn rlp_append(&self, s: &mut RlpStream) {
        s.append_list(&self.block_hashes);
    }
}

impl Decodable for NewBlockHashes {
    fn decode(d: &Rlp) -> Result<Self, DecoderError> {
        let block_hashes = d.as_list()?;
        Ok(NewBlockHashes { block_hashes })
    }
}

impl Handleable for NewBlockHashes {
    fn handle(self, ctx: &Context) -> Result<(), Error> {
        debug!("on_new_block_hashes, msg={:?}", self);

        if ctx.manager.catch_up_mode() {
            // If a node is in catch-up mode and we are not in test-mode, we
            // just simple ignore new block hashes.
            if ctx.manager.protocol_config.test_mode {
                if let Ok(info) = ctx.manager.syn.get_peer_info(&ctx.node_id) {
                    let mut info = info.write();
                    self.block_hashes.iter().for_each(|h| {
                        info.latest_block_hashes.insert(*h);
                    });
                }
            }
            return Ok(());
        }

        let headers_to_request = self
            .block_hashes
            .iter()
            .filter(|hash| {
                ctx.manager
                    .graph
                    .data_man
                    .block_header_by_hash(&hash)
                    .is_none()
            })
            .cloned()
            .collect::<Vec<_>>();

        ctx.manager.request_block_headers(
            ctx.io,
            Some(ctx.node_id.clone()),
            headers_to_request,
            // We have already checked db that these headers do not exist.
            true, /* ignore_db */
        );

        Ok(())
    }
}
