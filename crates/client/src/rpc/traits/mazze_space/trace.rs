// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::rpc::types::{
    EpochNumber, EpochTrace, LocalizedBlockTrace, LocalizedTrace, TraceFilter,
};
use mazze_rpc_error::Result as JsonRpcResult;
use jsonrpsee::proc_macros::rpc;
use mazze_types::H256;

/// Trace specific rpc interface.
#[rpc(server)]
pub trait Trace {
    /// Returns all traces produced at the given block.
    #[method(name = "trace_block", blocking)]
    fn block_traces(
        &self, block_hash: H256,
    ) -> JsonRpcResult<Option<LocalizedBlockTrace>>;

    /// Returns all traces matching the provided filter.
    #[method(name = "trace_filter", blocking)]
    fn filter_traces(
        &self, filter: TraceFilter,
    ) -> JsonRpcResult<Option<Vec<LocalizedTrace>>>;

    /// Returns all traces produced at the given transaction.
    #[method(name = "trace_transaction", blocking)]
    fn transaction_traces(
        &self, tx_hash: H256,
    ) -> JsonRpcResult<Option<Vec<LocalizedTrace>>>;

    /// Return all traces of both spaces in an epoch.
    #[method(name = "trace_epoch", blocking)]
    fn epoch_traces(&self, epoch: EpochNumber) -> JsonRpcResult<EpochTrace>;
}
