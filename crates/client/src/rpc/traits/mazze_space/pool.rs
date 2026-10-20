// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::rpc::types::{
    AccountPendingInfo, AccountPendingTransactions, RpcAddress,
    Transaction as RpcTransaction, TxPoolPendingNonceRange, TxPoolStatus,
    TxWithPoolInfo,
};
use mazze_rpc_error::{Result as JsonRpcResult};
use jsonrpsee::proc_macros::rpc;
use mazze_types::{H256, U256, U64};

/// Transaction pool RPCs
#[rpc(server)]
pub trait TransactionPool {
    #[method(name = "txpool_status", blocking)]
    fn txpool_status(&self) -> JsonRpcResult<TxPoolStatus>;

    #[method(name = "txpool_nextNonce", blocking)]
    fn txpool_next_nonce(&self, address: RpcAddress) -> JsonRpcResult<U256>;

    #[method(name = "txpool_transactionByAddressAndNonce", blocking)]
    fn txpool_transaction_by_address_and_nonce(
        &self, address: RpcAddress, nonce: U256,
    ) -> JsonRpcResult<Option<RpcTransaction>>;

    #[method(name = "txpool_pendingNonceRange", blocking)]
    fn txpool_pending_nonce_range(
        &self, address: RpcAddress,
    ) -> JsonRpcResult<TxPoolPendingNonceRange>;

    #[method(name = "txpool_txWithPoolInfo", blocking)]
    fn txpool_tx_with_pool_info(
        &self, hash: H256,
    ) -> JsonRpcResult<TxWithPoolInfo>;

    /// Get transaction pending info by account address
    #[method(name = "txpool_accountPendingInfo")]
    async fn account_pending_info(
        &self, address: RpcAddress,
    ) -> mazze_rpc_error::Result<Option<AccountPendingInfo>>;

    /// Get transaction pending info by account address
    #[method(name = "txpool_accountPendingTransactions")]
    async fn account_pending_transactions(
        &self, address: RpcAddress, maybe_start_nonce: Option<U256>,
        maybe_limit: Option<U64>,
    ) -> mazze_rpc_error::Result<AccountPendingTransactions>;
}
