// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::rpc::types::eth::{BlockNumber, LocalizedTrace, TraceFilter};
use jsonrpc_core::Result as JsonRpcResult;
use jsonrpc_derive::rpc;
use mazze_types::H256;

/// Trace specific rpc interface.
#[rpc(server)]
pub trait Trace {
    /// Returns all traces produced at the given block.
    #[rpc(name = "trace_block")]
    fn block_traces(
        &self, block_number: BlockNumber,
    ) -> JsonRpcResult<Option<Vec<LocalizedTrace>>>;

    /// Returns all traces matching the provided filter.
    #[rpc(name = "trace_filter")]
    fn filter_traces(
        &self, filter: TraceFilter,
    ) -> JsonRpcResult<Option<Vec<LocalizedTrace>>>;

    /// Returns all traces produced at the given transaction.
    #[rpc(name = "trace_transaction")]
    fn transaction_traces(
        &self, tx_hash: H256,
    ) -> JsonRpcResult<Option<Vec<LocalizedTrace>>>;
}
