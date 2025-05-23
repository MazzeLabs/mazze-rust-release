// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::{
    message::RequestId,
    sync::{
        message::{Context, GetTerminalBlockHashesResponse, Handleable},
        Error,
    },
};
use malloc_size_of_derive::MallocSizeOf as DeriveMallocSizeOf;
use rlp_derive::{RlpDecodable, RlpEncodable};

#[derive(Debug, PartialEq, RlpDecodable, RlpEncodable, DeriveMallocSizeOf)]
pub struct GetTerminalBlockHashes {
    pub request_id: RequestId,
}

impl Handleable for GetTerminalBlockHashes {
    fn handle(self, ctx: &Context) -> Result<(), Error> {
        let best_info = ctx.manager.graph.consensus.best_info();
        let terminal_hashes = best_info.bounded_terminal_block_hashes.clone();
        let response = GetTerminalBlockHashesResponse {
            request_id: self.request_id,
            hashes: terminal_hashes,
        };
        ctx.send_response(&response)
    }
}
