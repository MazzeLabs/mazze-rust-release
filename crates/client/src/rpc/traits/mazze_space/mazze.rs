// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::rpc::types::{
    pos::PoSEpochReward, Account as RpcAccount, AccountPendingInfo,
    AccountPendingTransactions, Block, BlockHashOrEpochNumber, Bytes,
    CallRequest, CheckBalanceAgainstTransactionResponse, EpochNumber,
    EstimateGasAndCollateralResponse, Log as RpcLog, MazzeFeeHistory,
    MazzeFilterChanges, MazzeRpcLogFilter, PoSEconomics, Receipt as RpcReceipt,
    RewardInfo as RpcRewardInfo, RpcAddress, SponsorInfo, Status as RpcStatus,
    StorageCollateralInfo, TokenSupplyInfo, Transaction, VoteParamsInfo,
    U64 as HexU64,
};
use jsonrpc_core::{BoxFuture, Result as JsonRpcResult};
use jsonrpc_derive::rpc;
use mazze_types::{H128, H256, U256, U64};
use primitives::{DepositInfo, StorageRoot, VoteStakeInfo};

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
    #[rpc(name = "mazze_gasPrice")]
    fn gas_price(&self) -> BoxFuture<U256>;

    /// Returns current max_priority_fee
    #[rpc(name = "mazze_maxPriorityFeePerGas")]
    fn max_priority_fee_per_gas(&self) -> BoxFuture<U256>;

    /// Returns highest epoch number.
    #[rpc(name = "mazze_epochNumber")]
    fn epoch_number(
        &self, epoch_number: Option<EpochNumber>,
    ) -> JsonRpcResult<U256>;

    /// Returns balance of the given account.
    #[rpc(name = "mazze_getBalance")]
    fn balance(
        &self, addr: RpcAddress,
        block_hash_or_epoch_number: Option<BlockHashOrEpochNumber>,
    ) -> BoxFuture<U256>;

    /// Returns admin of the given contract
    #[rpc(name = "mazze_getAdmin")]
    fn admin(
        &self, addr: RpcAddress, epoch_number: Option<EpochNumber>,
    ) -> BoxFuture<Option<RpcAddress>>;

    /// Returns sponsor information of the given contract
    #[rpc(name = "mazze_getSponsorInfo")]
    fn sponsor_info(
        &self, addr: RpcAddress, epoch_number: Option<EpochNumber>,
    ) -> BoxFuture<SponsorInfo>;

    /// Returns balance of the given account.
    #[rpc(name = "mazze_getStakingBalance")]
    fn staking_balance(
        &self, addr: RpcAddress, epoch_number: Option<EpochNumber>,
    ) -> BoxFuture<U256>;

    /// Returns deposit list of the given account.
    #[rpc(name = "mazze_getDepositList")]
    fn deposit_list(
        &self, addr: RpcAddress, epoch_number: Option<EpochNumber>,
    ) -> BoxFuture<Vec<DepositInfo>>;

    /// Returns vote list of the given account.
    #[rpc(name = "mazze_getVoteList")]
    fn vote_list(
        &self, addr: RpcAddress, epoch_number: Option<EpochNumber>,
    ) -> BoxFuture<Vec<VoteStakeInfo>>;

    /// Returns balance of the given account.
    #[rpc(name = "mazze_getCollateralForStorage")]
    fn collateral_for_storage(
        &self, addr: RpcAddress, epoch_number: Option<EpochNumber>,
    ) -> BoxFuture<U256>;

    /// Returns the code at given address at given time (epoch number).
    #[rpc(name = "mazze_getCode")]
    fn code(
        &self, addr: RpcAddress,
        block_hash_or_epoch_number: Option<BlockHashOrEpochNumber>,
    ) -> BoxFuture<Bytes>;

    /// Returns storage entries from a given contract.
    #[rpc(name = "mazze_getStorageAt")]
    fn storage_at(
        &self, addr: RpcAddress, pos: U256,
        block_hash_or_epoch_number: Option<BlockHashOrEpochNumber>,
    ) -> BoxFuture<Option<H256>>;

    #[rpc(name = "mazze_getStorageRoot")]
    fn storage_root(
        &self, address: RpcAddress, epoch_num: Option<EpochNumber>,
    ) -> BoxFuture<Option<StorageRoot>>;

    /// Returns block with given hash.
    #[rpc(name = "mazze_getBlockByHash")]
    fn block_by_hash(
        &self, block_hash: H256, include_txs: bool,
    ) -> BoxFuture<Option<Block>>;

    /// Returns block with given hash and main chain assumption.
    #[rpc(name = "mazze_getBlockByHashWithMainAssumption")]
    fn block_by_hash_with_main_assumption(
        &self, block_hash: H256, main_hash: H256, epoch_number: U64,
    ) -> BoxFuture<Block>;

    /// Returns block with given epoch number.
    #[rpc(name = "mazze_getBlockByEpochNumber")]
    fn block_by_epoch_number(
        &self, epoch_number: EpochNumber, include_txs: bool,
    ) -> BoxFuture<Option<Block>>;

    /// Returns block with given block number.
    #[rpc(name = "mazze_getBlockByBlockNumber")]
    fn block_by_block_number(
        &self, block_number: U64, include_txs: bool,
    ) -> BoxFuture<Option<Block>>;

    /// Returns best block hash.
    #[rpc(name = "mazze_getBestBlockHash")]
    fn best_block_hash(&self) -> JsonRpcResult<H256>;

    /// Returns the nonce should be filled in next sending transaction from
    /// given address at given time (epoch number).
    #[rpc(name = "mazze_getNextNonce")]
    fn next_nonce(
        &self, addr: RpcAddress, epoch_number: Option<BlockHashOrEpochNumber>,
    ) -> BoxFuture<U256>;

    //        /// Returns the number of transactions in a block with given hash.
    //        #[rpc(name = "mazze_getBlockTransactionCountByHash")]
    //        fn block_transaction_count_by_hash(&self, H256) ->
    // BoxFuture<Option<U256>>;

    //        /// Returns the number of transactions in a block with given block
    // number.        #[rpc(name = "mazze_getBlockTransactionCountByNumber")]
    //        fn block_trasaction_count_by_number(&self, BlockNumber) ->
    // BoxFuture<Option<U256>>;

    /// Sends signed transaction, returning its hash.
    #[rpc(name = "mazze_sendRawTransaction")]
    fn send_raw_transaction(&self, raw_tx: Bytes) -> JsonRpcResult<H256>;

    //        /// @alias of `mazze_sendRawTransaction`.
    //        #[rpc(name = "mazze_submitTransaction")]
    //        fn submit_transaction(&self, Bytes) -> JsonRpcResult<H256>;

    /// Call contract, returning the output data.
    #[rpc(name = "mazze_call")]
    fn call(
        &self, tx: CallRequest,
        block_hash_or_epoch_number: Option<BlockHashOrEpochNumber>,
    ) -> JsonRpcResult<Bytes>;

    /// Returns logs matching the filter provided.
    #[rpc(name = "mazze_getLogs")]
    fn get_logs(&self, filter: MazzeRpcLogFilter) -> BoxFuture<Vec<RpcLog>>;

    /// Get transaction by its hash.
    #[rpc(name = "mazze_getTransactionByHash")]
    fn transaction_by_hash(
        &self, tx_hash: H256,
    ) -> BoxFuture<Option<Transaction>>;

    /// Get transaction pending info by account address
    #[rpc(name = "mazze_getAccountPendingInfo")]
    fn account_pending_info(
        &self, address: RpcAddress,
    ) -> BoxFuture<Option<AccountPendingInfo>>;

    /// Get transaction pending info by account address
    #[rpc(name = "mazze_getAccountPendingTransactions")]
    fn account_pending_transactions(
        &self, address: RpcAddress, maybe_start_nonce: Option<U256>,
        maybe_limit: Option<U64>,
    ) -> BoxFuture<AccountPendingTransactions>;

    /// Return estimated gas and collateral usage.
    #[rpc(name = "mazze_estimateGasAndCollateral")]
    fn estimate_gas_and_collateral(
        &self, request: CallRequest, epoch_number: Option<EpochNumber>,
    ) -> JsonRpcResult<EstimateGasAndCollateralResponse>;

    #[rpc(name = "mazze_feeHistory")]
    fn fee_history(
        &self, block_count: HexU64, newest_block: EpochNumber,
        reward_percentiles: Vec<f64>,
    ) -> BoxFuture<MazzeFeeHistory>;

    /// Check if user balance is enough for the transaction.
    #[rpc(name = "mazze_checkBalanceAgainstTransaction")]
    fn check_balance_against_transaction(
        &self, account_addr: RpcAddress, contract_addr: RpcAddress,
        gas_limit: U256, gas_price: U256, storage_limit: U256,
        epoch: Option<EpochNumber>,
    ) -> BoxFuture<CheckBalanceAgainstTransactionResponse>;

    #[rpc(name = "mazze_getBlocksByEpoch")]
    fn blocks_by_epoch(
        &self, epoch_number: EpochNumber,
    ) -> JsonRpcResult<Vec<H256>>;

    #[rpc(name = "mazze_getSkippedBlocksByEpoch")]
    fn skipped_blocks_by_epoch(
        &self, epoch_number: EpochNumber,
    ) -> JsonRpcResult<Vec<H256>>;

    #[rpc(name = "mazze_getTransactionReceipt")]
    fn transaction_receipt(
        &self, tx_hash: H256,
    ) -> BoxFuture<Option<RpcReceipt>>;

    /// Return account related states of the given account
    #[rpc(name = "mazze_getAccount")]
    fn account(
        &self, address: RpcAddress, epoch_num: Option<EpochNumber>,
    ) -> BoxFuture<RpcAccount>;

    /// Returns interest rate of the given epoch
    #[rpc(name = "mazze_getInterestRate")]
    fn interest_rate(
        &self, epoch_number: Option<EpochNumber>,
    ) -> BoxFuture<U256>;

    /// Returns accumulate interest rate of the given epoch
    #[rpc(name = "mazze_getAccumulateInterestRate")]
    fn accumulate_interest_rate(
        &self, epoch_number: Option<EpochNumber>,
    ) -> BoxFuture<U256>;

    /// Returns accumulate interest rate of the given epoch
    #[rpc(name = "mazze_getPoSEconomics")]
    fn pos_economics(
        &self, epoch_number: Option<EpochNumber>,
    ) -> BoxFuture<PoSEconomics>;

    #[rpc(name = "mazze_getConfirmationRiskByHash")]
    fn confirmation_risk_by_hash(
        &self, block_hash: H256,
    ) -> JsonRpcResult<Option<U256>>;

    #[rpc(name = "mazze_getStatus")]
    fn get_status(&self) -> JsonRpcResult<RpcStatus>;

    /// Returns block reward information in an epoch
    #[rpc(name = "mazze_getBlockRewardInfo")]
    fn get_block_reward_info(
        &self, num: EpochNumber,
    ) -> JsonRpcResult<Vec<RpcRewardInfo>>;

    /// Return the client version as a string
    #[rpc(name = "mazze_clientVersion")]
    fn get_client_version(&self) -> JsonRpcResult<String>;

    /// Return information about total token supply.
    #[rpc(name = "mazze_getSupplyInfo")]
    fn get_supply_info(
        &self, epoch_number: Option<EpochNumber>,
    ) -> JsonRpcResult<TokenSupplyInfo>;

    /// Return information about total token supply.
    #[rpc(name = "mazze_getCollateralInfo")]
    fn get_collateral_info(
        &self, epoch_number: Option<EpochNumber>,
    ) -> JsonRpcResult<StorageCollateralInfo>;

    #[rpc(name = "mazze_getFeeBurnt")]
    fn get_fee_burnt(
        &self, epoch_number: Option<EpochNumber>,
    ) -> JsonRpcResult<U256>;

    #[rpc(name = "mazze_getParamsFromVote")]
    fn get_vote_params(
        &self, epoch_number: Option<EpochNumber>,
    ) -> JsonRpcResult<VoteParamsInfo>;

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
}

/// Eth filters rpc api (polling).
#[rpc(server)]
pub trait MazzeFilter {
    /// Returns id of new filter.
    #[rpc(name = "mazze_newFilter")]
    fn new_filter(&self, _: MazzeRpcLogFilter) -> JsonRpcResult<H128>;

    /// Returns id of new block filter.
    #[rpc(name = "mazze_newBlockFilter")]
    fn new_block_filter(&self) -> JsonRpcResult<H128>;

    /// Returns id of new block filter.
    #[rpc(name = "mazze_newPendingTransactionFilter")]
    fn new_pending_transaction_filter(&self) -> JsonRpcResult<H128>;

    /// Returns filter changes since last poll.
    #[rpc(name = "mazze_getFilterChanges")]
    fn filter_changes(&self, _: H128) -> JsonRpcResult<MazzeFilterChanges>;

    /// Returns all logs matching given filter (in a range 'from' - 'to').
    #[rpc(name = "mazze_getFilterLogs")]
    fn filter_logs(&self, _: H128) -> JsonRpcResult<Vec<RpcLog>>;

    /// Uninstalls filter.
    #[rpc(name = "mazze_uninstallFilter")]
    fn uninstall_filter(&self, _: H128) -> JsonRpcResult<bool>;
}
