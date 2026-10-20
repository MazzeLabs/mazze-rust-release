use crate::rpc::types::eth::{BlockNumber, CallRequest};
use alloy_rpc_types_trace::geth::{
    GethDebugTracingCallOptions, GethDebugTracingOptions, GethTrace,
    TraceResult,
};
use mazze_rpc_error::Result as JsonRpcResult;
use jsonrpsee::proc_macros::rpc;
use mazze_types::H256;

/// methods compatible with geth debug namespace methods https://geth.ethereum.org/docs/interacting-with-geth/rpc/ns-debug
#[rpc(server)]
pub trait Debug {
    #[method(name = "debug_dbGet", blocking)]
    fn db_get(&self, key: String) -> JsonRpcResult<Option<String>>;

    /// The `debug_traceTransaction` debugging method will attempt to run the
    /// transaction in the exact same manner as it was executed on the
    /// network. It will replay any transaction that may have been executed
    /// prior to this one before it will finally attempt to execute the
    /// transaction that corresponds to the given hash.
    #[method(name = "debug_traceTransaction", blocking)]
    fn debug_trace_transaction(
        &self, tx_hash: H256, opts: Option<GethDebugTracingOptions>,
    ) -> JsonRpcResult<GethTrace>;

    #[method(name = "debug_traceBlockByHash", blocking)]
    fn debug_trace_block_by_hash(
        &self, block: H256, opts: Option<GethDebugTracingOptions>,
    ) -> JsonRpcResult<Vec<TraceResult>>;

    #[method(name = "debug_traceBlockByNumber", blocking)]
    fn debug_trace_block_by_number(
        &self, block: BlockNumber, opts: Option<GethDebugTracingOptions>,
    ) -> JsonRpcResult<Vec<TraceResult>>;

    #[method(name = "debug_traceCall", blocking)]
    fn debug_trace_call(
        &self, request: CallRequest, block_number: Option<BlockNumber>,
        opts: Option<GethDebugTracingCallOptions>,
    ) -> JsonRpcResult<GethTrace>;
}
