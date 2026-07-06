// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::rpc::types::{
    BlockHashOrEpochNumber, Bytes as RpcBytes, ConsensusGraphStates,
    EpochNumber, MdbxShadowParityReport, Receipt as RpcReceipt,
    RpcAddress, SendTxRequest, StatOnGasLoad, SyncGraphStates,
    Transaction as RpcTransaction, WrapTransaction,
};
use jsonrpc_core::{BoxFuture, Result as JsonRpcResult};
use jsonrpc_derive::rpc;
use mazze_types::{H256, H520, U128, U64};
use mazzecore::verification::EpochReceiptProof;
use network::{
    node_table::{Node, NodeId},
    throttling, SessionDetails, UpdateNodeOperation,
};
use std::collections::BTreeMap;

#[rpc(server)]
pub trait LocalRpc {
    #[rpc(name = "txpool_inspect")]
    fn txpool_inspect(
        &self, address: Option<RpcAddress>,
    ) -> JsonRpcResult<
        BTreeMap<String, BTreeMap<String, BTreeMap<usize, Vec<String>>>>,
    >;

    // return all txpool transactions grouped by hex address
    #[rpc(name = "txpool_content")]
    fn txpool_content(
        &self, address: Option<RpcAddress>,
    ) -> JsonRpcResult<
        BTreeMap<
            String,
            BTreeMap<String, BTreeMap<usize, Vec<RpcTransaction>>>,
        >,
    >;

    // return account ready + deferred transactions
    #[rpc(name = "txpool_accountTransactions")]
    fn txpool_get_account_transactions(
        &self, address: RpcAddress,
    ) -> JsonRpcResult<Vec<RpcTransaction>>;

    #[rpc(name = "txpool_clear")]
    fn txpool_clear(&self) -> JsonRpcResult<()>;

    #[rpc(name = "net_throttling")]
    fn net_throttling(&self) -> JsonRpcResult<throttling::Service>;

    #[rpc(name = "net_node")]
    fn net_node(
        &self, node_id: NodeId,
    ) -> JsonRpcResult<Option<(String, Node)>>;

    #[rpc(name = "net_disconnect_node")]
    fn net_disconnect_node(
        &self, id: NodeId, op: Option<UpdateNodeOperation>,
    ) -> JsonRpcResult<bool>;

    #[rpc(name = "net_sessions")]
    fn net_sessions(
        &self, node_id: Option<NodeId>,
    ) -> JsonRpcResult<Vec<SessionDetails>>;

    #[rpc(name = "current_sync_phase")]
    fn current_sync_phase(&self) -> JsonRpcResult<String>;

    #[rpc(name = "consensus_graph_state")]
    fn consensus_graph_state(&self) -> JsonRpcResult<ConsensusGraphStates>;

    #[rpc(name = "sync_graph_state")]
    fn sync_graph_state(&self) -> JsonRpcResult<SyncGraphStates>;

    #[rpc(name = "mazze_sendTransaction")]
    fn send_transaction(
        &self, tx: SendTxRequest, password: Option<String>,
    ) -> BoxFuture<H256>;

    /// Returns accounts list.
    #[rpc(name = "accounts")]
    fn accounts(&self) -> JsonRpcResult<Vec<RpcAddress>>;

    /// Create a new account
    #[rpc(name = "new_account")]
    fn new_account(&self, password: String) -> JsonRpcResult<RpcAddress>;

    /// Unlock an account
    #[rpc(name = "unlock_account")]
    fn unlock_account(
        &self, address: RpcAddress, password: String, duration: Option<U128>,
    ) -> JsonRpcResult<bool>;

    /// Lock an account
    #[rpc(name = "lock_account")]
    fn lock_account(&self, address: RpcAddress) -> JsonRpcResult<bool>;

    #[rpc(name = "sign")]
    fn sign(
        &self, data: RpcBytes, address: RpcAddress, password: Option<String>,
    ) -> JsonRpcResult<H520>;

    #[rpc(name = "mazze_signTransaction")]
    fn sign_transaction(
        &self, tx: SendTxRequest, password: Option<String>,
    ) -> JsonRpcResult<String>;

    #[rpc(name = "mazze_getEpochReceipts")]
    fn epoch_receipts(
        &self, epoch: BlockHashOrEpochNumber,
        include_eth_recepits: Option<bool>,
    ) -> JsonRpcResult<Option<Vec<Vec<RpcReceipt>>>>;

    #[rpc(name = "debug_statOnGasLoad")]
    fn stat_on_gas_load(
        &self, last_epoch: EpochNumber, time_window: U64,
    ) -> JsonRpcResult<Option<StatOnGasLoad>>;

    #[rpc(name = "debug_getEpochReceiptProofByTransaction")]
    fn epoch_receipt_proof_by_transaction(
        &self, tx_hash: H256,
    ) -> JsonRpcResult<Option<EpochReceiptProof>>;

    #[rpc(name = "debug_getTransactionsByEpoch")]
    fn transactions_by_epoch(
        &self, epoch_number: U64,
    ) -> JsonRpcResult<Vec<WrapTransaction>>;

    #[rpc(name = "debug_getTransactionsByBlock")]
    fn transactions_by_block(
        &self, block_hash: H256,
    ) -> JsonRpcResult<Vec<WrapTransaction>>;

    /// Walk both sides of every active MDBX shadow mirror (Storage
    /// Phase 2) and return one divergence report per shadowed
    /// `DBTable`. Returns an empty vec when no mirror is installed
    /// — either every `enable_mdbx_shadow_*` flag is off in
    /// `hydra.toml` or the storage layer has no MDBX env open.
    /// Meant for an operator dashboard / era-boundary audit; not on
    /// the hot path — every `verify_parity` call materializes both
    /// columns into `Vec`s for the comparison.
    #[rpc(name = "debug_mdbxShadowVerifyParity")]
    fn mdbx_shadow_verify_parity(
        &self,
    ) -> JsonRpcResult<Vec<MdbxShadowParityReport>>;

    /// Flip the `ReadSource` for one shadowed `DBTable` at runtime
    /// (Storage Phase 3 cutover). `table` names one of the enum
    /// variants: `"Misc"`, `"Blocks"`, `"Transactions"`,
    /// `"EpochNumbers"`, `"BlamedHeaderVerifiedRoots"`,
    /// `"BlockTraces"`, `"HashByBlockNumber"`. `source` is one of
    /// `"primary"`, `"shadow_with_fallback"`, `"shadow"`.
    ///
    /// Returns `Ok(true)` when the mirror exists and the flip was
    /// applied, `Ok(false)` when the table isn't currently
    /// shadowed. Rejects unknown table / source strings with a
    /// JSON-RPC error so the caller can distinguish typo from
    /// no-op.
    ///
    /// Atomic swap inside the mirror — concurrent reads observe
    /// either the old or the new source, never a torn value.
    #[rpc(name = "debug_mdbxSetReadSource")]
    fn mdbx_set_read_source(
        &self, table: String, source: String,
    ) -> JsonRpcResult<bool>;

    /// Snapshot of every shadowed table's current `ReadSource`.
    /// Keys are table names (see `debug_mdbxSetReadSource` for the
    /// stable name set); values are one of `"primary"`,
    /// `"shadow_with_fallback"`, `"shadow"`. Empty object when no
    /// mirror is installed.
    #[rpc(name = "debug_mdbxGetReadSources")]
    fn mdbx_get_read_sources(
        &self,
    ) -> JsonRpcResult<std::collections::BTreeMap<String, String>>;
}
