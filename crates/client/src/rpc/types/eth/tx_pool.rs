use crate::rpc::types::eth::Transaction;
use mazze_types::U64;
use mazzecore::transaction_pool::TransactionStatus;

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountPendingTransactions {
    pub pending_transactions: Vec<Transaction>,
    pub first_tx_status: Option<TransactionStatus>,
    pub pending_count: U64,
}
