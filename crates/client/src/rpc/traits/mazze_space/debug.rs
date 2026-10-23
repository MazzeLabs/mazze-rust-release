// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::rpc::types::{
    BlockHashOrEpochNumber, Bytes as RpcBytes, ConsensusGraphStates,
    EpochNumber, Receipt as RpcReceipt,
    RpcAddress, SendTxRequest, StatOnGasLoad, SyncGraphStates,
    Transaction as RpcTransaction, WrapTransaction,
};
use mazze_rpc_error::{Result as JsonRpcResult};
use jsonrpsee::proc_macros::rpc;
use mazze_types::{H256, H520, U128, U64};
use mazzecore::verification::EpochReceiptProof;
use network::{
    node_table::{Node, NodeId},
    throttling, SessionDetails, UpdateNodeOperation,
};
use std::collections::BTreeMap;

#[rpc(server)]
pub trait LocalRpc {
    #[method(name = "txpool_inspect", blocking)]
    fn txpool_inspect(
        &self, address: Option<RpcAddress>,
    ) -> JsonRpcResult<
        BTreeMap<String, BTreeMap<String, BTreeMap<usize, Vec<String>>>>,
    >;

    // return all txpool transactions grouped by hex address
    #[method(name = "txpool_content", blocking)]
    fn txpool_content(
        &self, address: Option<RpcAddress>,
    ) -> JsonRpcResult<
        BTreeMap<
            String,
            BTreeMap<String, BTreeMap<usize, Vec<RpcTransaction>>>,
        >,
    >;

    // return account ready + deferred transactions
    #[method(name = "txpool_accountTransactions", blocking)]
    fn txpool_get_account_transactions(
        &self, address: RpcAddress,
    ) -> JsonRpcResult<Vec<RpcTransaction>>;

    #[method(name = "txpool_clear", blocking)]
    fn txpool_clear(&self) -> JsonRpcResult<()>;

    #[method(name = "net_throttling", blocking)]
    fn net_throttling(&self) -> JsonRpcResult<throttling::Service>;

    #[method(name = "net_node", blocking)]
    fn net_node(
        &self, node_id: NodeId,
    ) -> JsonRpcResult<Option<(String, Node)>>;

    #[method(name = "net_disconnect_node", blocking)]
    fn net_disconnect_node(
        &self, id: NodeId, op: Option<UpdateNodeOperation>,
    ) -> JsonRpcResult<bool>;

    #[method(name = "net_sessions", blocking)]
    fn net_sessions(
        &self, node_id: Option<NodeId>,
    ) -> JsonRpcResult<Vec<SessionDetails>>;

    #[method(name = "current_sync_phase", blocking)]
    fn current_sync_phase(&self) -> JsonRpcResult<String>;

    /// `(receipts_root, state_root)` this node computed for the epoch whose
    /// main block is `block_hash` -- the test RPC `getExecutedInfo`, which
    /// release builds do not serve, on the debug API (local RPC port), so
    /// `scripts/compare-executed-state.py` works on a fleet.
    #[method(name = "debug_getExecutedInfo", blocking)]
    fn debug_get_executed_info(
        &self, block_hash: H256,
    ) -> JsonRpcResult<(H256, H256)>;

    #[method(name = "consensus_graph_state", blocking)]
    fn consensus_graph_state(&self) -> JsonRpcResult<ConsensusGraphStates>;

    #[method(name = "sync_graph_state", blocking)]
    fn sync_graph_state(&self) -> JsonRpcResult<SyncGraphStates>;

    #[method(name = "mazze_sendTransaction")]
    async fn send_transaction(
        &self, tx: SendTxRequest, password: Option<String>,
    ) -> mazze_rpc_error::Result<H256>;

    /// Returns accounts list.
    #[method(name = "accounts", blocking)]
    fn accounts(&self) -> JsonRpcResult<Vec<RpcAddress>>;

    /// Create a new account
    #[method(name = "new_account", blocking)]
    fn new_account(&self, password: String) -> JsonRpcResult<RpcAddress>;

    /// Unlock an account
    #[method(name = "unlock_account", blocking)]
    fn unlock_account(
        &self, address: RpcAddress, password: String, duration: Option<U128>,
    ) -> JsonRpcResult<bool>;

    /// Lock an account
    #[method(name = "lock_account", blocking)]
    fn lock_account(&self, address: RpcAddress) -> JsonRpcResult<bool>;

    #[method(name = "sign", blocking)]
    fn sign(
        &self, data: RpcBytes, address: RpcAddress, password: Option<String>,
    ) -> JsonRpcResult<H520>;

    #[method(name = "mazze_signTransaction", blocking)]
    fn sign_transaction(
        &self, tx: SendTxRequest, password: Option<String>,
    ) -> JsonRpcResult<String>;

    #[method(name = "mazze_getEpochReceipts", blocking)]
    fn epoch_receipts(
        &self, epoch: BlockHashOrEpochNumber,
        include_eth_recepits: Option<bool>,
    ) -> JsonRpcResult<Option<Vec<Vec<RpcReceipt>>>>;

    #[method(name = "debug_statOnGasLoad", blocking)]
    fn stat_on_gas_load(
        &self, last_epoch: EpochNumber, time_window: U64,
    ) -> JsonRpcResult<Option<StatOnGasLoad>>;

    #[method(name = "debug_getEpochReceiptProofByTransaction", blocking)]
    fn epoch_receipt_proof_by_transaction(
        &self, tx_hash: H256,
    ) -> JsonRpcResult<Option<EpochReceiptProof>>;

    #[method(name = "debug_getTransactionsByEpoch", blocking)]
    fn transactions_by_epoch(
        &self, epoch_number: U64,
    ) -> JsonRpcResult<Vec<WrapTransaction>>;

    #[method(name = "debug_getTransactionsByBlock", blocking)]
    fn transactions_by_block(
        &self, block_hash: H256,
    ) -> JsonRpcResult<Vec<WrapTransaction>>;
}
