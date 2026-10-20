// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::rpc::types::{
    Account as RpcAccount, AccountPendingInfo, AccountPendingTransactions,
    Block, BlockHashOrEpochNumber, Bytes, CallRequest,
    CheckBalanceAgainstTransactionResponse, EpochNumber,
    EstimateGasAndCollateralResponse, Log as RpcLog, MazzeFeeHistory,
    MazzeFilterChanges, MazzeRpcLogFilter, Receipt as RpcReceipt,
    RewardInfo as RpcRewardInfo, RpcAddress, SponsorInfo, Status as RpcStatus,
    StorageCollateralInfo, TokenSupplyInfo, Transaction, U64 as HexU64,
};
use mazze_rpc_error::{Result as JsonRpcResult, Value as JsonValue};
use jsonrpsee::proc_macros::rpc;
use mazze_types::{H128, H256, U256, U64};
use primitives::StorageRoot;

/// Mazze rpc interface.
#[rpc(server)]
pub trait Mazze {
    //        /// Returns protocol version encoded as a string (quotes are
    // necessary).        #[rpc(name = "mazze_protocolVersion")]
    //        fn protocol_version(&self) -> JsonRpcResult<String>;
    //
    /// Returns the number of hashes per second that the node is mining with.
    //        #[rpc(name = "mazze_hashrate")]
    //        fn hashrate(&self) -> JsonRpcResult<U256>;

    //        /// Returns block author.
    //        #[rpc(name = "mazze_coinbase")]
    //        fn author(&self) -> JsonRpcResult<H160>;

    //        /// Returns true if client is actively mining new blocks.
    //        #[rpc(name = "mazze_mining")]
    //        fn is_mining(&self) -> JsonRpcResult<bool>;

    /// Returns current gas price.
    #[method(name = "mazze_gasPrice")]
    async fn gas_price(&self) -> mazze_rpc_error::Result<U256>;

    /// Returns current max_priority_fee
    #[method(name = "mazze_maxPriorityFeePerGas")]
    async fn max_priority_fee_per_gas(&self) -> mazze_rpc_error::Result<U256>;

    /// Returns highest epoch number.
    #[method(name = "mazze_epochNumber", blocking)]
    fn epoch_number(
        &self, epoch_number: Option<EpochNumber>,
    ) -> JsonRpcResult<U256>;

    /// Returns balance of the given account.
    #[method(name = "mazze_getBalance")]
    async fn balance(
        &self, addr: RpcAddress,
        block_hash_or_epoch_number: Option<BlockHashOrEpochNumber>,
    ) -> mazze_rpc_error::Result<U256>;

    /// Returns admin of the given contract
    #[method(name = "mazze_getAdmin")]
    async fn admin(
        &self, addr: RpcAddress, epoch_number: Option<EpochNumber>,
    ) -> mazze_rpc_error::Result<Option<RpcAddress>>;

    /// Returns sponsor information of the given contract
    #[method(name = "mazze_getSponsorInfo")]
    async fn sponsor_info(
        &self, addr: RpcAddress, epoch_number: Option<EpochNumber>,
    ) -> mazze_rpc_error::Result<SponsorInfo>;

    /// Returns balance of the given account.
    #[method(name = "mazze_getCollateralForStorage")]
    async fn collateral_for_storage(
        &self, addr: RpcAddress, epoch_number: Option<EpochNumber>,
    ) -> mazze_rpc_error::Result<U256>;

    /// Returns the code at given address at given time (epoch number).
    #[method(name = "mazze_getCode")]
    async fn code(
        &self, addr: RpcAddress,
        block_hash_or_epoch_number: Option<BlockHashOrEpochNumber>,
    ) -> mazze_rpc_error::Result<Bytes>;

    /// Returns storage entries from a given contract.
    #[method(name = "mazze_getStorageAt")]
    async fn storage_at(
        &self, addr: RpcAddress, pos: U256,
        block_hash_or_epoch_number: Option<BlockHashOrEpochNumber>,
    ) -> mazze_rpc_error::Result<Option<H256>>;

    #[method(name = "mazze_getStorageRoot")]
    async fn storage_root(
        &self, address: RpcAddress, epoch_num: Option<EpochNumber>,
    ) -> mazze_rpc_error::Result<Option<StorageRoot>>;

    /// Returns block with given hash.
    #[method(name = "mazze_getBlockByHash")]
    async fn block_by_hash(
        &self, block_hash: H256, include_txs: bool,
    ) -> mazze_rpc_error::Result<Option<Block>>;

    /// Returns block with given hash and main chain assumption.
    #[method(name = "mazze_getBlockByHashWithMainAssumption")]
    async fn block_by_hash_with_main_assumption(
        &self, block_hash: H256, main_hash: H256, epoch_number: U64,
    ) -> mazze_rpc_error::Result<Block>;

    /// Returns block with given epoch number.
    #[method(name = "mazze_getBlockByEpochNumber")]
    async fn block_by_epoch_number(
        &self, epoch_number: EpochNumber, include_txs: bool,
    ) -> mazze_rpc_error::Result<Option<Block>>;

    /// Returns block with given block number.
    #[method(name = "mazze_getBlockByBlockNumber")]
    async fn block_by_block_number(
        &self, block_number: U64, include_txs: bool,
    ) -> mazze_rpc_error::Result<Option<Block>>;

    /// Returns best block hash.
    #[method(name = "mazze_getBestBlockHash", blocking)]
    fn best_block_hash(&self) -> JsonRpcResult<H256>;

    /// Returns the nonce should be filled in next sending transaction from
    /// given address at given time (epoch number).
    #[method(name = "mazze_getNextNonce")]
    async fn next_nonce(
        &self, addr: RpcAddress, epoch_number: Option<BlockHashOrEpochNumber>,
    ) -> mazze_rpc_error::Result<U256>;

    //        /// Returns the number of transactions in a block with given hash.
    //        #[rpc(name = "mazze_getBlockTransactionCountByHash")]
    //        fn block_transaction_count_by_hash(&self, H256) ->
    // BoxFuture<Option<U256>>;

    //        /// Returns the number of transactions in a block with given block
    // number.        #[rpc(name = "mazze_getBlockTransactionCountByNumber")]
    //        fn block_trasaction_count_by_number(&self, BlockNumber) ->
    // BoxFuture<Option<U256>>;

    /// Sends signed transaction, returning its hash.
    #[method(name = "mazze_sendRawTransaction", blocking)]
    fn send_raw_transaction(&self, raw_tx: Bytes) -> JsonRpcResult<H256>;

    //        /// @alias of `mazze_sendRawTransaction`.
    //        #[rpc(name = "mazze_submitTransaction")]
    //        fn submit_transaction(&self, Bytes) -> JsonRpcResult<H256>;

    /// Call contract, returning the output data.
    #[method(name = "mazze_call", blocking)]
    fn call(
        &self, tx: CallRequest,
        block_hash_or_epoch_number: Option<BlockHashOrEpochNumber>,
    ) -> JsonRpcResult<Bytes>;

    /// Returns logs matching the filter provided.
    #[method(name = "mazze_getLogs")]
    async fn get_logs(&self, filter: MazzeRpcLogFilter) -> mazze_rpc_error::Result<Vec<RpcLog>>;

    /// Get transaction by its hash.
    #[method(name = "mazze_getTransactionByHash")]
    async fn transaction_by_hash(
        &self, tx_hash: H256,
    ) -> mazze_rpc_error::Result<Option<Transaction>>;

    /// Get transaction pending info by account address
    #[method(name = "mazze_getAccountPendingInfo")]
    async fn account_pending_info(
        &self, address: RpcAddress,
    ) -> mazze_rpc_error::Result<Option<AccountPendingInfo>>;

    /// Get transaction pending info by account address
    #[method(name = "mazze_getAccountPendingTransactions")]
    async fn account_pending_transactions(
        &self, address: RpcAddress, maybe_start_nonce: Option<U256>,
        maybe_limit: Option<U64>,
    ) -> mazze_rpc_error::Result<AccountPendingTransactions>;

    /// Return estimated gas and collateral usage.
    #[method(name = "mazze_estimateGasAndCollateral", blocking)]
    fn estimate_gas_and_collateral(
        &self, request: CallRequest, epoch_number: Option<EpochNumber>,
    ) -> JsonRpcResult<EstimateGasAndCollateralResponse>;

    #[method(name = "mazze_feeHistory")]
    async fn fee_history(
        &self, block_count: HexU64, newest_block: EpochNumber,
        reward_percentiles: Vec<f64>,
    ) -> mazze_rpc_error::Result<MazzeFeeHistory>;

    /// Check if user balance is enough for the transaction.
    #[method(name = "mazze_checkBalanceAgainstTransaction")]
    async fn check_balance_against_transaction(
        &self, account_addr: RpcAddress, contract_addr: RpcAddress,
        gas_limit: U256, gas_price: U256, storage_limit: U256,
        epoch: Option<EpochNumber>,
    ) -> mazze_rpc_error::Result<CheckBalanceAgainstTransactionResponse>;

    #[method(name = "mazze_getBlocksByEpoch", blocking)]
    fn blocks_by_epoch(
        &self, epoch_number: EpochNumber,
    ) -> JsonRpcResult<Vec<H256>>;

    #[method(name = "mazze_getSkippedBlocksByEpoch", blocking)]
    fn skipped_blocks_by_epoch(
        &self, epoch_number: EpochNumber,
    ) -> JsonRpcResult<Vec<H256>>;

    #[method(name = "mazze_getTransactionReceipt")]
    async fn transaction_receipt(
        &self, tx_hash: H256,
    ) -> mazze_rpc_error::Result<Option<RpcReceipt>>;

    /// Return account related states of the given account
    #[method(name = "mazze_getAccount")]
    async fn account(
        &self, address: RpcAddress, epoch_num: Option<EpochNumber>,
    ) -> mazze_rpc_error::Result<RpcAccount>;

    #[method(name = "mazze_getConfirmationRiskByHash", blocking)]
    fn confirmation_risk_by_hash(
        &self, block_hash: H256,
    ) -> JsonRpcResult<Option<U256>>;

    #[method(name = "mazze_getStatus", blocking)]
    fn get_status(&self) -> JsonRpcResult<RpcStatus>;

    /// Returns block reward information in an epoch
    #[method(name = "mazze_getBlockRewardInfo", blocking)]
    fn get_block_reward_info(
        &self, num: EpochNumber,
    ) -> JsonRpcResult<Vec<RpcRewardInfo>>;

    /// Return the client version as a string
    #[method(name = "mazze_clientVersion", blocking)]
    fn get_client_version(&self) -> JsonRpcResult<String>;

    /// Return the node's version as structured data: node software version,
    /// build commit and consensus version (peers with a different non-zero
    /// consensus version are rejected at handshake).
    #[method(name = "mazze_nodeVersion", blocking)]
    fn get_node_version(&self) -> JsonRpcResult<JsonValue>;

    /// Diagnostic: checkpoint machinery state (era / stable heights, timer
    /// chain and force-confirm point), to see why checkpoints do or do not
    /// form on a node.
    #[method(name = "mazze_consensusDebug", blocking)]
    fn get_consensus_debug(&self) -> JsonRpcResult<JsonValue>;

    /// Return information about total token supply.
    #[method(name = "mazze_getSupplyInfo", blocking)]
    fn get_supply_info(
        &self, epoch_number: Option<EpochNumber>,
    ) -> JsonRpcResult<TokenSupplyInfo>;

    #[method(name = "mazze_getCollateralInfo", blocking)]
    fn get_collateral_info(
        &self, epoch_number: Option<EpochNumber>,
    ) -> JsonRpcResult<StorageCollateralInfo>;

    #[method(name = "mazze_getFeeBurnt", blocking)]
    fn get_fee_burnt(
        &self, epoch_number: Option<EpochNumber>,
    ) -> JsonRpcResult<U256>;

    //        /// Returns transaction at given block hash and index.
    //        #[rpc(name = "mazze_getTransactionByBlockHashAndIndex")]
    //        fn transaction_by_block_hash_and_index(&self, H256, Index) ->
    // BoxFuture<Option<Transaction>>;

    //        /// Returns transaction by given block number and index.
    //        #[rpc(name = "mazze_getTransactionByBlockNumberAndIndex")]
    //        fn transaction_by_block_number_and_index(&self, BlockNumber,
    // Index) -> BoxFuture<Option<Transaction>>;

    //        /// Returns uncles at given block and index.
    //        #[rpc(name = "mazze_getUnclesByBlockHashAndIndex")]
    //        fn uncles_by_block_hash_and_index(&self, H256, Index) ->
    // BoxFuture<Option<Block>>;

    //        /// Returns uncles at given block and index.
    //        #[rpc(name = "mazze_getUnclesByBlockNumberAndIndex")]
    //        fn uncles_by_block_number_and_index(&self, BlockNumber, Index) ->
    // BoxFuture<Option<Block>>;

    /// Check if a block is a timer block
    #[method(name = "mazze_isTimerBlock", blocking)]
    fn is_timer_block(&self, block_hash: H256) -> JsonRpcResult<bool>;

    /// Get the timer chain hash list
    #[method(name = "mazze_getTimerChain", blocking)]
    fn get_timer_chain(&self) -> JsonRpcResult<Vec<H256>>;

    /// Get the timer chain difficulty
    #[method(name = "mazze_getTimerChainDifficulty", blocking)]
    fn get_timer_chain_difficulty(&self) -> JsonRpcResult<U256>;

    /// Get block blame info by block hash
    #[method(name = "mazze_getBlockBlameInfo", blocking)]
    fn get_block_blame_info(
        &self, block_hash: H256,
    ) -> JsonRpcResult<JsonValue>;

    /// Get current DAG tips (terminal block hashes)
    #[method(name = "mazze_getDagTips", blocking)]
    fn get_dag_tips(&self) -> JsonRpcResult<Vec<H256>>;

    /// Alias for skipped blocks by epoch
    #[method(name = "mazze_getSkippedBlockHashesByEpoch", blocking)]
    fn get_skipped_block_hashes_by_epoch(
        &self, epoch_number: EpochNumber,
    ) -> JsonRpcResult<Vec<H256>>;

    /// Get RandomX epoch info
    #[method(name = "mazze_getRandomXEpochInfo", blocking)]
    fn get_randomx_epoch_info(
        &self, epoch_number: Option<EpochNumber>,
    ) -> JsonRpcResult<JsonValue>;

    /// Get era details (latest_era supported)
    #[method(name = "mazze_getEraDetails", blocking)]
    fn get_era_details(
        &self, selector: Option<String>,
    ) -> JsonRpcResult<JsonValue>;

    /// Get block weight by hash (WLSR weight)
    #[method(name = "mazze_getBlockWeight", blocking)]
    fn get_block_weight(&self, block_hash: H256) -> JsonRpcResult<U256>;

    /// Check if a block is adaptive
    #[method(name = "mazze_isAdaptiveBlock", blocking)]
    fn is_adaptive_block(&self, block_hash: H256) -> JsonRpcResult<bool>;

    /// Check if a block is partial invalid
    #[method(name = "mazze_isPartialInvalid", blocking)]
    fn is_partial_invalid(&self, block_hash: H256) -> JsonRpcResult<bool>;
}

/// Eth filters rpc api (polling).
#[rpc(server)]
pub trait MazzeFilter {
    /// Returns id of new filter.
    #[method(name = "mazze_newFilter", blocking)]
    fn new_filter(&self, arg0: MazzeRpcLogFilter) -> JsonRpcResult<H128>;

    /// Returns id of new block filter.
    #[method(name = "mazze_newBlockFilter", blocking)]
    fn new_block_filter(&self) -> JsonRpcResult<H128>;

    /// Returns id of new block filter.
    #[method(name = "mazze_newPendingTransactionFilter", blocking)]
    fn new_pending_transaction_filter(&self) -> JsonRpcResult<H128>;

    /// Returns filter changes since last poll.
    #[method(name = "mazze_getFilterChanges", blocking)]
    fn filter_changes(&self, arg0: H128) -> JsonRpcResult<MazzeFilterChanges>;

    /// Returns all logs matching given filter (in a range 'from' - 'to').
    #[method(name = "mazze_getFilterLogs", blocking)]
    fn filter_logs(&self, arg0: H128) -> JsonRpcResult<Vec<RpcLog>>;

    /// Uninstalls filter.
    #[method(name = "mazze_uninstallFilter", blocking)]
    fn uninstall_filter(&self, arg0: H128) -> JsonRpcResult<bool>;
}
