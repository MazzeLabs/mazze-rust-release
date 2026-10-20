// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

// Copyright 2015-2020 Parity Technologies (UK) Ltd.
// This file is part of OpenEthereum.

// OpenEthereum is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// OpenEthereum is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with OpenEthereum.  If not, see <http://www.gnu.org/licenses/>.

//! Eth rpc interface.
use crate::rpc::types::U64 as HexU64;
use mazze_rpc_error::Result;
use jsonrpsee::proc_macros::rpc;
use mazze_types::{H128, H160, H256, U256, U64};

use crate::rpc::types::{
    eth::{
        AccountPendingTransactions, Block, BlockNumber, CallRequest,
        EthRpcLogFilter, FilterChanges, Log, Receipt, SyncStatus, Transaction,
    },
    Bytes, FeeHistory, Index,
};

/// Eth rpc interface.
#[rpc(server)]
pub trait Eth {
    #[method(name = "web3_clientVersion", blocking)]
    fn client_version(&self) -> Result<String>;

    #[method(name = "net_version", blocking)]
    fn net_version(&self) -> Result<String>;

    /// Returns protocol version encoded as a string (quotes are necessary).
    #[method(name = "eth_protocolVersion", blocking)]
    fn protocol_version(&self) -> Result<String>;

    /// Returns an object with data about the sync status or false. (wtf?)
    #[method(name = "eth_syncing", blocking)]
    fn syncing(&self) -> Result<SyncStatus>;

    /// Returns the number of hashes per second that the node is mining with.
    #[method(name = "eth_hashrate", blocking)]
    fn hashrate(&self) -> Result<U256>;

    /// Returns block author.
    #[method(name = "eth_coinbase", blocking)]
    fn author(&self) -> Result<H160>;

    /// Returns true if client is actively mining new blocks.
    #[method(name = "eth_mining", blocking)]
    fn is_mining(&self) -> Result<bool>;

    /// Returns the chain ID used for transaction signing at the
    /// current best block. None is returned if not
    /// available.
    #[method(name = "eth_chainId", blocking)]
    fn chain_id(&self) -> Result<Option<U64>>;

    /// Returns current gas_price.
    #[method(name = "eth_gasPrice", blocking)]
    fn gas_price(&self) -> Result<U256>;

    /// Returns current max_priority_fee
    #[method(name = "eth_maxPriorityFeePerGas", blocking)]
    fn max_priority_fee_per_gas(&self) -> Result<U256>;

    #[method(name = "eth_feeHistory", blocking)]
    fn fee_history(
        &self, block_count: HexU64, newest_block: BlockNumber,
        reward_percentiles: Vec<f64>,
    ) -> Result<FeeHistory>;

    /// Returns accounts list.
    #[method(name = "eth_accounts", blocking)]
    fn accounts(&self) -> Result<Vec<H160>>;

    /// Returns highest block number.
    #[method(name = "eth_blockNumber", blocking)]
    fn block_number(&self) -> Result<U256>;

    /// Returns balance of the given account.
    #[method(name = "eth_getBalance", blocking)]
    fn balance(
        &self, address: H160, block: Option<BlockNumber>,
    ) -> Result<U256>;

    // /// Returns the account- and storage-values of the specified account
    // including the Merkle-proof #[rpc(name = "eth_getProof")]
    // fn proof(&self, _: H160, _: Vec<H256>, _: Option<BlockNumber>) ->
    // BoxFuture<EthAccount>;

    /// Returns content of the storage at given address.
    #[method(name = "eth_getStorageAt", blocking)]
    fn storage_at(
        &self, address: H160, storage_slot: U256, block: Option<BlockNumber>,
    ) -> mazze_rpc_error::Result<H256>;

    /// Returns block with given hash.
    #[method(name = "eth_getBlockByHash", blocking)]
    fn block_by_hash(
        &self, block_hash: H256, hydrated_transactions: bool,
    ) -> Result<Option<Block>>;

    /// Returns block with given number.
    #[method(name = "eth_getBlockByNumber", blocking)]
    fn block_by_number(
        &self, block: BlockNumber, hydrated_transactions: bool,
    ) -> Result<Option<Block>>;

    /// Returns the number of transactions sent from given address at given time
    /// (block number).
    #[method(name = "eth_getTransactionCount", blocking)]
    fn transaction_count(
        &self, address: H160, block: Option<BlockNumber>,
    ) -> Result<U256>;

    /// Returns the number of transactions in a block with given hash.
    #[method(name = "eth_getBlockTransactionCountByHash", blocking)]
    fn block_transaction_count_by_hash(
        &self, block_hash: H256,
    ) -> Result<Option<U256>>;

    /// Returns the number of transactions in a block with given block number.
    #[method(name = "eth_getBlockTransactionCountByNumber", blocking)]
    fn block_transaction_count_by_number(
        &self, block: BlockNumber,
    ) -> Result<Option<U256>>;

    /// Returns the number of uncles in a block with given hash.
    #[method(name = "eth_getUncleCountByBlockHash", blocking)]
    fn block_uncles_count_by_hash(
        &self, block_hash: H256,
    ) -> Result<Option<U256>>;

    /// Returns the number of uncles in a block with given block number.
    #[method(name = "eth_getUncleCountByBlockNumber", blocking)]
    fn block_uncles_count_by_number(
        &self, block: BlockNumber,
    ) -> Result<Option<U256>>;

    /// Returns the code at given address at given time (block number).
    #[method(name = "eth_getCode", blocking)]
    fn code_at(
        &self, address: H160, block: Option<BlockNumber>,
    ) -> Result<Bytes>;

    /// Sends signed transaction, returning its hash.
    #[method(name = "eth_sendRawTransaction", blocking)]
    fn send_raw_transaction(&self, transaction: Bytes) -> Result<H256>;

    /// @alias of `eth_sendRawTransaction`.
    #[method(name = "eth_submitTransaction", blocking)]
    fn submit_transaction(&self, transaction: Bytes) -> Result<H256>;

    /// Call contract, returning the output data.
    #[method(name = "eth_call", blocking)]
    fn call(
        &self, transaction: CallRequest, block: Option<BlockNumber>,
    ) -> Result<Bytes>;

    /// Estimate gas needed for execution of given contract.
    #[method(name = "eth_estimateGas", blocking)]
    fn estimate_gas(
        &self, transaction: CallRequest, block: Option<BlockNumber>,
    ) -> Result<U256>;

    /// Get transaction by its hash.
    #[method(name = "eth_getTransactionByHash", blocking)]
    fn transaction_by_hash(
        &self, transaction_hash: H256,
    ) -> Result<Option<Transaction>>;

    /// Returns transaction at given block hash and index.
    #[method(name = "eth_getTransactionByBlockHashAndIndex", blocking)]
    fn transaction_by_block_hash_and_index(
        &self, block_hash: H256, transaction_index: Index,
    ) -> Result<Option<Transaction>>;

    /// Returns transaction by given block number and index.
    #[method(name = "eth_getTransactionByBlockNumberAndIndex", blocking)]
    fn transaction_by_block_number_and_index(
        &self, block: BlockNumber, transaction_index: Index,
    ) -> Result<Option<Transaction>>;

    /// Returns transaction receipt by transaction hash.
    #[method(name = "eth_getTransactionReceipt", blocking)]
    fn transaction_receipt(
        &self, transaction_hash: H256,
    ) -> Result<Option<Receipt>>;

    /// Returns an uncles at given block and index.
    #[method(name = "eth_getUncleByBlockHashAndIndex", blocking)]
    fn uncle_by_block_hash_and_index(
        &self, block_hash: H256, arg0: Index,
    ) -> Result<Option<Block>>;

    /// Returns an uncles at given block and index.
    #[method(name = "eth_getUncleByBlockNumberAndIndex", blocking)]
    fn uncle_by_block_number_and_index(
        &self, block: BlockNumber, arg0: Index,
    ) -> Result<Option<Block>>;

    // /// Returns available compilers.
    // /// @deprecated
    // #[rpc(name = "eth_getCompilers")]
    // fn compilers(&self) -> Result<Vec<String>>;
    //
    // /// Compiles lll code.
    // /// @deprecated
    // #[rpc(name = "eth_compileLLL")]
    // fn compile_lll(&self, _: String) -> Result<Bytes>;
    //
    // /// Compiles solidity.
    // /// @deprecated
    // #[rpc(name = "eth_compileSolidity")]
    // fn compile_solidity(&self, _: String) -> Result<Bytes>;
    //
    // /// Compiles serpent.
    // /// @deprecated
    // #[rpc(name = "eth_compileSerpent")]
    // fn compile_serpent(&self, _: String) -> Result<Bytes>;

    /// Returns logs matching given filter object.
    #[method(name = "eth_getLogs", blocking)]
    fn logs(&self, filter: EthRpcLogFilter) -> Result<Vec<Log>>;

    // /// Returns the hash of the current block, the seedHash, and the boundary
    // condition to be met. #[rpc(name = "eth_getWork")]
    // fn work(&self, _: Option<u64>) -> Result<Work>;

    // /// Used for submitting a proof-of-work solution.
    // #[rpc(name = "eth_submitWork")]
    // fn submit_work(&self, _: H64, _: H256, _: H256) -> Result<bool>;

    /// Used for submitting mining hashrate.
    #[method(name = "eth_submitHashrate", blocking)]
    fn submit_hashrate(&self, arg0: U256, arg1: H256) -> Result<bool>;

    #[method(name = "parity_getBlockReceipts", blocking)]
    fn block_receipts(
        &self, block: Option<BlockNumber>,
    ) -> Result<Vec<Receipt>>;

    #[method(name = "eth_getAccountPendingTransactions", blocking)]
    fn account_pending_transactions(
        &self, address: H160, maybe_start_nonce: Option<U256>,
        maybe_limit: Option<U64>,
    ) -> Result<AccountPendingTransactions>;
}

/// Eth filters rpc api (polling).
#[rpc(server)]
pub trait EthFilter {
    /// Returns id of new filter.
    #[method(name = "eth_newFilter", blocking)]
    fn new_filter(&self, filter: EthRpcLogFilter) -> Result<H128>;

    /// Returns id of new block filter.
    #[method(name = "eth_newBlockFilter", blocking)]
    fn new_block_filter(&self) -> Result<H128>;

    /// Returns id of new block filter.
    #[method(name = "eth_newPendingTransactionFilter", blocking)]
    fn new_pending_transaction_filter(&self) -> Result<H128>;

    /// Returns filter changes since last poll.
    #[method(name = "eth_getFilterChanges", blocking)]
    fn filter_changes(&self, identifier: H128) -> Result<FilterChanges>;

    /// Returns all logs matching given filter (in a range 'from' - 'to').
    #[method(name = "eth_getFilterLogs", blocking)]
    fn filter_logs(&self, identifier: H128) -> Result<Vec<Log>>;

    /// Uninstalls filter.
    #[method(name = "eth_uninstallFilter", blocking)]
    fn uninstall_filter(&self, identifier: H128) -> Result<bool>;
}
