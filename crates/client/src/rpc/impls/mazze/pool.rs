// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::{
    common::delegate_convert,
    rpc::{
        impls::common::RpcImpl as CommonImpl,
        traits::pool::TransactionPoolServer,
        types::{
            AccountPendingInfo, AccountPendingTransactions, RpcAddress,
            Transaction as RpcTransaction, TxPoolPendingNonceRange,
            TxPoolStatus, TxWithPoolInfo,
        },
    },
};
use delegate::delegate;
use mazze_rpc_error::Result as JsonRpcResult;
use mazze_types::{H256, U256, U64};
use std::sync::Arc;

pub struct TransactionPoolHandler {
    common: Arc<CommonImpl>,
}

impl TransactionPoolHandler {
    pub fn new(common: Arc<CommonImpl>) -> Self {
        TransactionPoolHandler { common }
    }
}

impl TransactionPoolServer for TransactionPoolHandler {
    delegate! {
        to self.common {
            fn txpool_status(&self) -> JsonRpcResult<TxPoolStatus>;
            fn txpool_next_nonce(&self, address: RpcAddress) -> JsonRpcResult<U256>;
            fn txpool_pending_nonce_range(&self, address: RpcAddress) -> JsonRpcResult<TxPoolPendingNonceRange>;
            fn txpool_tx_with_pool_info(&self, hash: H256) -> JsonRpcResult<TxWithPoolInfo>;
            fn txpool_transaction_by_address_and_nonce(&self, address: RpcAddress, nonce: U256) -> JsonRpcResult<Option<RpcTransaction>>;
            async fn account_pending_info(&self, addr: RpcAddress) -> mazze_rpc_error::Result<Option<AccountPendingInfo>>;
            async fn account_pending_transactions(&self, address: RpcAddress, maybe_start_nonce: Option<U256>, maybe_limit: Option<U64>) -> mazze_rpc_error::Result<AccountPendingTransactions>;
        }
    }
}
