// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

mod account_cache;
mod garbage_collector;
mod nonce_pool;
mod transaction_pool_inner;

extern crate rand;

pub use self::transaction_pool_inner::TransactionStatus;
use crate::{
    block_data_manager::BlockDataManager,
    consensus::BestInformation,
    transaction_pool::{
        nonce_pool::TxWithReadyInfo, transaction_pool_inner::PendingReason,
    },
    verification::{VerificationConfig, VerifyTxLocalMode, VerifyTxMode},
};
use account_cache::AccountCache;
use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};
use mazze_executor::{
    machine::Machine, spec::TransitionsEpochHeight, state::State,
};
use mazze_parameters::{
    block::DEFAULT_TARGET_BLOCK_GAS_LIMIT,
    consensus_internal::ELASTICITY_MULTIPLIER,
};
use mazze_statedb::{Result as StateDbResult, StateDb};
use mazze_storage::{StateIndex, StorageManagerTrait};
use mazze_types::{
    AddressWithSpace as Address, AllChainID, Space, SpaceMap, H256, U256,
};
use mazze_vm_types::Spec;
use metrics::{
    register_meter_with_group, Gauge, GaugeUsize, Lock, Meter, MeterTimer,
    RwLockExtensions,
};
use parking_lot::{Mutex, RwLock};
use primitives::{
    block::BlockHeight, block_header::compute_next_price_tuple, Account,
    SignedTransaction, Transaction, TransactionWithSignature,
};
use std::{
    cmp::{max, min},
    collections::{hash_map::HashMap, BTreeSet},
    mem,
    ops::DerefMut,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use transaction_pool_inner::TransactionPoolInner;

lazy_static! {
    static ref TX_POOL_DEFERRED_GAUGE: Arc<dyn Gauge<usize>> =
        GaugeUsize::register_with_group("txpool", "stat_deferred_txs");
    static ref TX_POOL_UNPACKED_GAUGE: Arc<dyn Gauge<usize>> =
        GaugeUsize::register_with_group("txpool", "stat_unpacked_txs");
    static ref TX_POOL_READY_GAUGE: Arc<dyn Gauge<usize>> =
        GaugeUsize::register_with_group("txpool", "stat_ready_accounts");
    static ref INSERT_TPS: Arc<dyn Meter> =
        register_meter_with_group("txpool", "insert_tps");
    static ref INSERT_TXS_TPS: Arc<dyn Meter> =
        register_meter_with_group("txpool", "insert_txs_tps");
    static ref INSERT_TXS_SUCCESS_TPS: Arc<dyn Meter> =
        register_meter_with_group("txpool", "insert_txs_success_tps");
    static ref INSERT_TXS_FAILURE_TPS: Arc<dyn Meter> =
        register_meter_with_group("txpool", "insert_txs_failure_tps");
    static ref TX_POOL_INSERT_TIMER: Arc<dyn Meter> =
        register_meter_with_group("timer", "tx_pool::insert_new_tx");
    static ref TX_POOL_VERIFY_TIMER: Arc<dyn Meter> =
        register_meter_with_group("timer", "tx_pool::verify");
    static ref TX_POOL_GET_STATE_TIMER: Arc<dyn Meter> =
        register_meter_with_group("timer", "tx_pool::get_state");
    static ref INSERT_TXS_QUOTA_LOCK: Lock =
        Lock::register("txpool_insert_txs_quota_lock");
    static ref INSERT_TXS_ENQUEUE_LOCK: Lock =
        Lock::register("txpool_insert_txs_enqueue_lock");
    static ref PACK_TRANSACTION_LOCK: Lock =
        Lock::register("txpool_pack_transactions");
    static ref NOTIFY_BEST_INFO_LOCK: Lock =
        Lock::register("txpool_notify_best_info");
    static ref NOTIFY_MODIFIED_LOCK: Lock =
        Lock::register("txpool_notify_modified_info");
}

pub struct TxPoolConfig {
    pub capacity: usize,
    pub min_native_tx_price: u64,
    pub min_eth_tx_price: u64,
    pub max_tx_gas: RwLock<U256>,
    pub packing_gas_limit_block_count: u64,
    pub target_block_gas_limit: u64,
    pub max_packing_batch_gas_limit: u64,
    pub max_packing_batch_size: usize,
    pub packing_pool_degree: u8,
}

impl MallocSizeOf for TxPoolConfig {
    fn size_of(&self, _ops: &mut MallocSizeOfOps) -> usize {
        0
    }
}

impl Default for TxPoolConfig {
    fn default() -> Self {
        TxPoolConfig {
            capacity: 500_000,
            min_native_tx_price: 1,
            min_eth_tx_price: 1,
            max_tx_gas: RwLock::new(U256::from(
                DEFAULT_TARGET_BLOCK_GAS_LIMIT / 2,
            )),
            packing_gas_limit_block_count: 10,
            max_packing_batch_size: 20,
            max_packing_batch_gas_limit: DEFAULT_TARGET_BLOCK_GAS_LIMIT / 10,
            packing_pool_degree: 4,
            target_block_gas_limit: DEFAULT_TARGET_BLOCK_GAS_LIMIT,
        }
    }
}

pub struct TransactionPool {
    pub config: TxPoolConfig,
    verification_config: VerificationConfig,
    inner: RwLock<TransactionPoolInner>,
    to_propagate_trans: Arc<RwLock<HashMap<H256, Arc<SignedTransaction>>>>,
    pub data_man: Arc<BlockDataManager>,
    best_executed_state: Mutex<Arc<State>>,
    consensus_best_info: Mutex<Arc<BestInformation>>,
    set_tx_requests: Mutex<Vec<Arc<SignedTransaction>>>,
    recycle_tx_requests: Mutex<Vec<Arc<SignedTransaction>>>,
    machine: Arc<Machine>,

    /// If it's `false`, operations on the tx pool will be ignored to save
    /// memory/CPU cost.
    ready_for_mining: AtomicBool,
}

impl MallocSizeOf for TransactionPool {
    fn size_of(&self, ops: &mut MallocSizeOfOps) -> usize {
        let inner_size = self.inner.read().size_of(ops);
        let to_propagate_trans_size =
            self.to_propagate_trans.read().size_of(ops);
        let consensus_best_info_size =
            self.consensus_best_info.lock().size_of(ops);
        let set_tx_requests_size = self.set_tx_requests.lock().size_of(ops);
        let recycle_tx_requests_size =
            self.recycle_tx_requests.lock().size_of(ops);
        self.config.size_of(ops)
            + inner_size
            + to_propagate_trans_size
            + self.data_man.size_of(ops)
            + consensus_best_info_size
            + set_tx_requests_size
            + recycle_tx_requests_size
        // Does not count size_of machine
    }
}

pub type SharedTransactionPool = Arc<TransactionPool>;

impl TransactionPool {
    pub fn new(
        config: TxPoolConfig, verification_config: VerificationConfig,
        data_man: Arc<BlockDataManager>, machine: Arc<Machine>,
    ) -> Self {
        let genesis_hash = data_man.true_genesis.hash();
        let inner = TransactionPoolInner::new(
            config.capacity,
            config.max_packing_batch_gas_limit as usize,
            config.max_packing_batch_size,
            config.packing_pool_degree,
        );
        let best_executed_state = Mutex::new(
            Self::best_executed_state(
                &data_man,
                StateIndex::new_for_readonly(
                    &genesis_hash,
                    &data_man.true_genesis_state_root(),
                ),
            )
            .expect("The genesis state is guaranteed to exist."),
        );
        TransactionPool {
            config,
            verification_config,
            inner: RwLock::new(inner),
            to_propagate_trans: Arc::new(RwLock::new(HashMap::new())),
            data_man: data_man.clone(),
            best_executed_state,
            consensus_best_info: Mutex::new(Arc::new(Default::default())),
            set_tx_requests: Mutex::new(Default::default()),
            recycle_tx_requests: Mutex::new(Default::default()),
            machine,
            ready_for_mining: AtomicBool::new(false),
        }
    }

    pub fn machine(&self) -> Arc<Machine> {
        self.machine.clone()
    }

    pub fn get_transaction(
        &self, tx_hash: &H256,
    ) -> Option<Arc<SignedTransaction>> {
        self.inner.read().get(tx_hash)
    }

    pub fn get_transaction_by_address2nonce(
        &self, address: Address, nonce: U256,
    ) -> Option<Arc<SignedTransaction>> {
        self.inner.read().get_by_address2nonce(address, nonce)
    }

    pub fn check_tx_packed_in_deferred_pool(&self, tx_hash: &H256) -> bool {
        self.inner.read().check_tx_packed_in_deferred_pool(tx_hash)
    }

    pub fn get_local_account_info(&self, address: &Address) -> (U256, U256) {
        self.inner
            .read()
            .get_local_nonce_and_balance(address)
            .unwrap_or((0.into(), 0.into()))
    }

    pub fn get_next_nonce(&self, address: &Address) -> U256 {
        let (state_nonce, _) = self
            .get_state_account_info(address)
            .unwrap_or((0.into(), 0.into()));
        self.inner.read().get_next_nonce(address, state_nonce)
    }

    pub fn get_account_pending_info(
        &self, address: &Address,
    ) -> Option<(U256, U256, U256, H256)> {
        self.inner.read().get_account_pending_info(address)
    }

    /// Return `(pending_txs, first_tx_status, pending_count)`.
    pub fn get_account_pending_transactions(
        &self, address: &Address, maybe_start_nonce: Option<U256>,
        maybe_limit: Option<usize>, best_height: BlockHeight,
    ) -> (
        Vec<Arc<SignedTransaction>>,
        Option<TransactionStatus>,
        usize,
    ) {
        let inner = self.inner.read();
        let (txs, mut first_tx_status, pending_count) = inner
            .get_account_pending_transactions(
                address,
                maybe_start_nonce,
                maybe_limit,
            );
        if txs.is_empty() {
            return (txs, first_tx_status, pending_count);
        }

        let first_tx = txs.first().expect("non empty");
        if address.space == Space::Native {
            if let Transaction::Native(tx) = &first_tx.unsigned {
                if VerificationConfig::check_transaction_epoch_bound(
                    tx,
                    best_height,
                    self.verification_config.transaction_epoch_bound,
                ) == -1
                {
                    // If the epoch height is out of bound, overwrite the
                    // pending reason.
                    first_tx_status = Some(TransactionStatus::Pending(
                        PendingReason::OldEpochHeight,
                    ));
                }
            }
        }

        if matches!(
            first_tx_status,
            Some(TransactionStatus::Ready)
                | Some(TransactionStatus::Pending(
                    PendingReason::NotEnoughCash
                ))
        ) {
            // The sponsor status may have changed, check again.
            // This is not applied to the tx pool state because this check is
            // only triggered on the RPC server.
            let account_cache = self.get_best_state_account_cache();
            match inner.get_sponsored_gas_and_storage(&account_cache, &first_tx)
            {
                Ok((sponsored_gas, sponsored_storage)) => {
                    if let Ok((_, balance)) =
                        account_cache.get_nonce_and_balance(&first_tx.sender())
                    {
                        let tx_info = TxWithReadyInfo::new(
                            first_tx.clone(),
                            false,
                            sponsored_gas,
                            sponsored_storage,
                        );
                        if tx_info.calc_tx_cost() <= balance {
                            // The tx should have been ready now.
                            if matches!(
                                first_tx_status,
                                Some(TransactionStatus::Pending(
                                    PendingReason::NotEnoughCash
                                ))
                            ) {
                                first_tx_status =
                                    Some(TransactionStatus::Pending(
                                        PendingReason::OutdatedStatus,
                                    ));
                            }
                        } else {
                            if matches!(
                                first_tx_status,
                                Some(TransactionStatus::Ready)
                            ) {
                                first_tx_status =
                                    Some(TransactionStatus::Pending(
                                        PendingReason::OutdatedStatus,
                                    ));
                            }
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "error in get_account_pending_transactions: e={:?}",
                        e
                    );
                }
            }
        }
        (txs, first_tx_status, pending_count)
    }

    pub fn get_pending_transaction_hashes_in_evm_pool(&self) -> BTreeSet<H256> {
        self.inner.read().ready_transacton_hashes_in_evm_pool()
    }

    pub fn get_pending_transaction_hashes_in_native_pool(
        &self,
    ) -> BTreeSet<H256> {
        self.inner.read().ready_transacton_hashes_in_native_pool()
    }

    pub fn get_state_account_info(
        &self, address: &Address,
    ) -> StateDbResult<(U256, U256)> {
        let account_cache = self.get_best_state_account_cache();
        account_cache.get_nonce_and_balance(address)
    }

    pub fn calc_max_tx_gas(&self) -> U256 {
        let current_best_info = self.consensus_best_info.lock().clone();
        match self
            .data_man
            .block_from_db(&current_best_info.best_block_hash)
        {
            Some(main_block) => main_block.block_header.gas_limit() / 2,
            None => *self.config.max_tx_gas.read(),
        }
    }

    /// Try to insert `transactions` into transaction pool.
    ///
    /// If some tx is already in our tx_cache, it will be ignored and will not
    /// be added to returned `passed_transactions`. If some tx invalid or
    /// cannot be inserted to the tx pool, it will be included in the returned
    /// `failure` and will not be propagated.
    pub fn insert_new_transactions(
        &self, mut transactions: Vec<TransactionWithSignature>,
    ) -> (Vec<Arc<SignedTransaction>>, HashMap<H256, String>) {
        INSERT_TPS.mark(1);
        INSERT_TXS_TPS.mark(transactions.len());
        let _timer = MeterTimer::time_func(TX_POOL_INSERT_TIMER.as_ref());

        let mut passed_transactions = Vec::new();
        let mut failure = HashMap::new();
        let current_best_info = self.consensus_best_info.lock().clone();

        let (chain_id, best_height, best_block_number) = {
            (
                current_best_info.best_chain_id(),
                current_best_info.best_epoch_number,
                current_best_info.best_block_number,
            )
        };
        // FIXME: Needs further discussion here, some transactions may be valid
        // and invalid back and forth does this matters? But for the epoch
        // height check, it may also become valid and invalid back and forth.
        let vm_spec = self.machine.spec(best_block_number, best_height);
        let transitions = &self.machine.params().transition_heights;

        // filter out invalid transactions.
        let mut index = 0;
        while let Some(tx) = transactions.get(index) {
            match self.verify_transaction_tx_pool(
                tx,
                /* basic_check = */ true,
                chain_id,
                best_height,
                transitions,
                &vm_spec,
            ) {
                Ok(_) => index += 1,
                Err(e) => {
                    let removed = transactions.swap_remove(index);
                    debug!("failed to insert tx into pool (validation failed), hash = {:?}, error = {:?}", removed.hash, e);
                    failure.insert(removed.hash, e);
                }
            }
        }

        if transactions.is_empty() {
            INSERT_TXS_SUCCESS_TPS.mark(passed_transactions.len());
            INSERT_TXS_FAILURE_TPS.mark(failure.len());
            return (passed_transactions, failure);
        }

        // Recover public key and insert into pool with readiness check.
        // Note, the workload of recovering public key is very heavy, especially
        // in case of high TPS (e.g. > 8000). So, it's better to recover public
        // key after basic verification.
        match self.data_man.recover_unsigned_tx(&transactions) {
            Ok(signed_trans) => {
                let account_cache = self.get_best_state_account_cache();
                let mut inner =
                    self.inner.write_with_metric(&INSERT_TXS_ENQUEUE_LOCK);
                let mut to_prop = self.to_propagate_trans.write();

                for tx in signed_trans {
                    if inner.get(&tx.hash).is_none() {
                        if let Err(e) = self
                            .add_transaction_with_readiness_check(
                                &mut *inner,
                                &account_cache,
                                tx.clone(),
                                false,
                                false,
                            )
                        {
                            debug!(
                            "tx {:?} fails to be inserted to pool, err={:?}",
                            &tx.hash, e
                        );
                            failure.insert(tx.hash(), e);
                            continue;
                        }
                        passed_transactions.push(tx.clone());
                        if !to_prop.contains_key(&tx.hash)
                            && to_prop.len() < inner.capacity()
                        {
                            to_prop.insert(tx.hash, tx);
                        }
                    }
                }
            }
            Err(e) => {
                for tx in transactions {
                    failure.insert(tx.hash(), format!("{:?}", e).into());
                }
            }
        }

        TX_POOL_DEFERRED_GAUGE.update(self.total_deferred(None));
        TX_POOL_UNPACKED_GAUGE.update(self.total_unpacked());
        TX_POOL_READY_GAUGE.update(self.total_ready_accounts());

        INSERT_TXS_SUCCESS_TPS.mark(passed_transactions.len());
        INSERT_TXS_FAILURE_TPS.mark(failure.len());

        (passed_transactions, failure)
    }

    /// Try to insert `signed_transaction` into transaction pool.
    ///
    /// If some tx is already in our tx_cache, it will be ignored and will not
    /// be added to returned `passed_transactions`. If some tx invalid or
    /// cannot be inserted to the tx pool, it will be included in the returned
    /// `failure` and will not be propagated.
    pub fn insert_new_signed_transactions(
        &self, mut signed_transactions: Vec<Arc<SignedTransaction>>,
    ) -> (Vec<Arc<SignedTransaction>>, HashMap<H256, String>) {
        INSERT_TPS.mark(1);
        INSERT_TXS_TPS.mark(signed_transactions.len());
        let _timer = MeterTimer::time_func(TX_POOL_INSERT_TIMER.as_ref());

        let mut passed_transactions = Vec::new();
        let mut failure = HashMap::new();
        let current_best_info = { self.consensus_best_info.lock().clone() };

        // filter out invalid transactions.
        let mut index = 0;

        let (chain_id, best_height, best_block_number) = {
            (
                current_best_info.best_chain_id(),
                current_best_info.best_epoch_number,
                current_best_info.best_block_number,
            )
        };
        // FIXME: Needs further discussion here, some transactions may be valid
        // and invalid back and forth does this matters?
        let vm_spec = self.machine.spec(best_block_number, best_height);
        let transitions = &self.machine.params().transition_heights;

        while let Some(tx) = signed_transactions.get(index) {
            match self.verify_transaction_tx_pool(
                &tx.transaction,
                true, /* basic_check = */
                chain_id,
                best_height,
                transitions,
                &vm_spec,
            ) {
                Ok(_) => index += 1,
                Err(e) => {
                    let removed = signed_transactions.swap_remove(index);
                    debug!("failed to insert tx into pool (validation failed), hash = {:?}, error = {:?}", removed.hash, e);
                    failure.insert(removed.hash, e);
                }
            }
        }

        // ensure the pool has enough quota to insert new signed transactions.
        let quota = self
            .inner
            .write_with_metric(&INSERT_TXS_QUOTA_LOCK)
            .remaining_quota();
        if quota < signed_transactions.len() {
            for tx in signed_transactions.split_off(quota) {
                trace!("failed to insert tx into pool (quota not enough), hash = {:?}", tx.hash);
                failure.insert(tx.hash, "txpool is full".into());
            }
        }

        if signed_transactions.is_empty() {
            INSERT_TXS_SUCCESS_TPS.mark(passed_transactions.len());
            INSERT_TXS_FAILURE_TPS.mark(failure.len());
            return (passed_transactions, failure);
        }

        // Insert into pool with readiness check.
        // Notice it does not recover the public as the input transactions are
        // already signed.

        {
            let account_cache = self.get_best_state_account_cache();
            let mut inner =
                self.inner.write_with_metric(&INSERT_TXS_ENQUEUE_LOCK);
            let mut to_prop = self.to_propagate_trans.write();

            for tx in signed_transactions {
                if let Err(e) = self.add_transaction_with_readiness_check(
                    &mut *inner,
                    &account_cache,
                    tx.clone(),
                    false,
                    false,
                ) {
                    debug!(
                        "tx {:?} fails to be inserted to pool, err={:?}",
                        &tx.hash, e
                    );
                    failure.insert(tx.hash(), e);
                    continue;
                }
                passed_transactions.push(tx.clone());
                if !to_prop.contains_key(&tx.hash) {
                    to_prop.insert(tx.hash, tx);
                }
            }
            //RwLock is dropped here
        }

        TX_POOL_DEFERRED_GAUGE.update(self.total_deferred(None));
        TX_POOL_UNPACKED_GAUGE.update(self.total_unpacked());
        TX_POOL_READY_GAUGE.update(self.total_ready_accounts());

        INSERT_TXS_SUCCESS_TPS.mark(passed_transactions.len());
        INSERT_TXS_FAILURE_TPS.mark(failure.len());

        (passed_transactions, failure)
    }

    /// verify transactions based on the rules that have nothing to do with
    /// readiness
    fn verify_transaction_tx_pool(
        &self, transaction: &TransactionWithSignature, basic_check: bool,
        chain_id: AllChainID, best_height: u64,
        transitions: &TransitionsEpochHeight, spec: &Spec,
    ) -> Result<(), String> {
        let _timer = MeterTimer::time_func(TX_POOL_VERIFY_TIMER.as_ref());
        let mode = VerifyTxMode::Local(VerifyTxLocalMode::MaybeLater, spec);

        if basic_check {
            self.verification_config
                .check_tx_size(transaction)
                .map_err(|e| e.to_string())?;
            if let Err(e) = self.verification_config.verify_transaction_common(
                transaction,
                chain_id,
                best_height,
                transitions,
                mode,
            ) {
                warn!("Transaction {:?} discarded due to not passing basic verification.", transaction.hash());
                return Err(format!("{:?}", e));
            }
        }

        // Check the epoch height is moved to verify_transaction_common. In
        // VerifyTxLocalMode::MaybeLater mode, a transaction with larger target
        // epoch can be accepted. Since PR #1610, it is guaranteed that
        // best info is initialized here.

        // check transaction gas limit
        let max_tx_gas = *self.config.max_tx_gas.read();
        if *transaction.gas() > max_tx_gas {
            warn!(
                "Transaction discarded due to above gas limit: {} > {:?}",
                transaction.gas(),
                max_tx_gas
            );
            return Err(format!(
                "transaction gas {} exceeds the maximum value {:?}, the half of main block gas limit",
                transaction.gas(), max_tx_gas
            ));
        }

        let min_tx_price = match transaction.space() {
            Space::Native => self.config.min_native_tx_price,
            Space::Ethereum => self.config.min_eth_tx_price,
        };
        // check transaction gas price
        if *transaction.gas_price() < min_tx_price.into() {
            trace!("Transaction {} discarded due to below minimal gas price: price {}", transaction.hash(), transaction.gas_price());
            return Err(format!(
                "transaction gas price {} less than the minimum value {}",
                transaction.gas_price(),
                min_tx_price
            ));
        }

        Ok(())
    }

    // Add transaction into deferred pool and maintain its readiness
    // the packed tag provided
    // if force tag is true, the replacement in nonce pool must be happened
    pub fn add_transaction_with_readiness_check(
        &self, inner: &mut TransactionPoolInner, account_cache: &AccountCache,
        transaction: Arc<SignedTransaction>, packed: bool, force: bool,
    ) -> Result<(), String> {
        inner.insert_transaction_with_readiness_check(
            account_cache,
            transaction,
            packed,
            force,
        )
    }

    pub fn get_to_be_propagated_transactions(
        &self,
    ) -> HashMap<H256, Arc<SignedTransaction>> {
        let mut to_prop = self.to_propagate_trans.write();
        let mut res = HashMap::new();
        mem::swap(&mut *to_prop, &mut res);
        res
    }

    pub fn set_to_be_propagated_transactions(
        &self, transactions: HashMap<H256, Arc<SignedTransaction>>,
    ) {
        let mut to_prop = self.to_propagate_trans.write();
        to_prop.extend(transactions);
    }

    pub fn remove_to_be_propagated_transactions(&self, tx_hash: &H256) {
        self.to_propagate_trans.write().remove(tx_hash);
    }

    // If a tx is failed executed due to invalid nonce or if its enclosing block
    // becomes orphan due to era transition. This function should be invoked
    // to recycle it
    pub fn recycle_transactions(
        &self, transactions: Vec<Arc<SignedTransaction>>,
    ) {
        trace!(
            "To re-add transactions to transaction pool. \
             transactions={:?}",
            &transactions
        );
        if transactions.is_empty() || !self.ready_for_mining() {
            // Fast return.
            return;
        }

        let mut recycle_req_buffer = self.recycle_tx_requests.lock();
        for tx in transactions {
            recycle_req_buffer.push(tx);
        }
    }

    pub fn set_tx_packed(&self, transactions: &Vec<Arc<SignedTransaction>>) {
        if transactions.is_empty() || !self.ready_for_mining() {
            // Fast return.
            return;
        }
        let mut tx_req_buffer = self.set_tx_requests.lock();
        for tx in transactions {
            tx_req_buffer.push(tx.clone());
        }
    }

    pub fn pack_transactions<'a>(
        &self, num_txs: usize, block_gas_limit: U256, evm_gas_limit: U256,
        block_size_limit: usize, mut best_epoch_height: u64,
        mut best_block_number: u64,
    ) -> Vec<Arc<SignedTransaction>> {
        let mut inner = self.inner.write_with_metric(&PACK_TRANSACTION_LOCK);
        best_epoch_height += 1;
        // The best block number is not necessary an exact number.
        best_block_number += 1;
        inner.pack_transactions(
            num_txs,
            block_gas_limit,
            evm_gas_limit,
            block_size_limit,
            best_epoch_height,
            best_block_number,
            &self.verification_config,
            &self.machine,
        )
    }

    pub fn pack_transactions_1559<'a>(
        &self, num_txs: usize, block_gas_limit: U256,
        parent_base_price: SpaceMap<U256>, block_size_limit: usize,
        mut best_epoch_height: u64, mut best_block_number: u64,
    ) -> (Vec<Arc<SignedTransaction>>, SpaceMap<U256>) {
        let mut inner = self.inner.write_with_metric(&PACK_TRANSACTION_LOCK);
        best_epoch_height += 1;
        // The best block number is not necessary an exact number.
        best_block_number += 1;

        let spec = self.machine.spec(best_block_number, best_epoch_height);
        let transitions = &self.machine.params().transition_heights;

        let validity = |tx: &SignedTransaction| {
            self.verification_config.fast_recheck(
                tx,
                best_epoch_height,
                transitions,
                &spec,
            )
        };

        inner.pack_transactions_1559(
            num_txs,
            block_gas_limit,
            parent_base_price,
            block_size_limit,
            best_epoch_height,
            &self.machine,
            validity,
        )
    }

    // A helper function for python test. Not intented to be used in the
    // production mode because of its inefficiency
    // LINT: this function should not belongs to txpool, since it does not
    // access the pools. However, since transaction pool has context for
    // computing the base price, it is the most proper position at this
    // time. May be fixed in future refactoring.
    pub fn compute_1559_base_price<'a, I>(
        &self, parent_hash: &H256, block_gas_limit: U256, txs: I,
    ) -> Result<Option<SpaceMap<U256>>, String>
    where
        I: Iterator<Item = &'a SignedTransaction> + 'a,
    {
        let parent = self
            .data_man
            .block_header_by_hash(parent_hash)
            .ok_or("Cannot find parent block")?;
        let current_height = parent.height() + 1;

        let params = self.machine.params();
        let mut gas_used = SpaceMap::default();
        let mut min_gas_price =
            SpaceMap::new(U256::max_value(), U256::max_value());
        for tx in txs {
            gas_used[tx.space()] += *tx.gas_limit();
            min_gas_price[tx.space()] =
                min_gas_price[tx.space()].min(*tx.gas_limit());
        }

        let core_gas_limit = block_gas_limit * 9 / 10;
        let eth_gas_limit = if params.can_pack_evm_transaction(current_height) {
            block_gas_limit * 5 / 10
        } else {
            U256::zero()
        };
        let gas_target =
            SpaceMap::new(core_gas_limit, eth_gas_limit).map_all(|x| x / 2);

        let parent_base_price = if current_height == 0 {
            params.init_base_price()
        } else {
            parent.base_price().unwrap()
        };

        let min_base_price = params.min_base_price();

        let base_price = SpaceMap::zip4(
            gas_target,
            gas_used,
            parent_base_price,
            min_base_price,
        )
        .map_all(compute_next_price_tuple);

        for space in [Space::Native, Space::Ethereum] {
            if base_price[space] > min_gas_price[space] {
                return Err(format!("Not sufficient min price in space {:?}, expected {:?}, actual {:?}", space, base_price[space], min_gas_price[space]));
            }
        }

        Ok(Some(base_price))
    }

    pub fn notify_modified_accounts(
        &self, accounts_from_execution: Vec<Account>,
    ) {
        let mut inner = self.inner.write_with_metric(&NOTIFY_MODIFIED_LOCK);
        inner.notify_modified_accounts(accounts_from_execution)
    }

    pub fn clear_tx_pool(&self) {
        let mut inner = self.inner.write();
        inner.clear()
    }

    pub fn total_deferred(&self, space: Option<Space>) -> usize {
        let inner = self.inner.read();
        inner.total_deferred(space)
    }

    pub fn total_ready_accounts(&self) -> usize {
        let inner = self.inner.read();
        inner.total_ready_accounts()
    }

    pub fn total_received(&self) -> usize {
        let inner = self.inner.read();
        inner.total_received()
    }

    pub fn total_unpacked(&self) -> usize {
        let inner = self.inner.read();
        inner.total_unpacked()
    }

    /// stats retrieves the length of ready and deferred pool.
    pub fn stats(&self) -> (usize, usize, usize, usize) {
        let inner = self.inner.read();
        (
            inner.total_ready_accounts(),
            inner.total_deferred(None),
            inner.total_received(),
            inner.total_unpacked(),
        )
    }

    /// content retrieves the ready and deferred transactions.
    pub fn content(
        &self, address: Option<Address>,
    ) -> (Vec<Arc<SignedTransaction>>, Vec<Arc<SignedTransaction>>) {
        let inner = self.inner.read();
        inner.content(address)
    }

    pub fn notify_new_best_info(
        &self, best_info: Arc<BestInformation>,
    ) -> StateDbResult<()> {
        let mut set_tx_buffer = self.set_tx_requests.lock();
        let mut recycle_tx_buffer = self.recycle_tx_requests.lock();
        {
            let mut consensus_best_info = self.consensus_best_info.lock();
            *consensus_best_info = best_info.clone();
        }
        *self.config.max_tx_gas.write() = self.calc_max_tx_gas();

        let account_cache = self.get_best_state_account_cache();
        let mut inner = self.inner.write_with_metric(&NOTIFY_BEST_INFO_LOCK);
        let inner = inner.deref_mut();

        while let Some(tx) = set_tx_buffer.pop() {
            let tx_hash = tx.hash();
            if let Err(e) = self.add_transaction_with_readiness_check(
                inner,
                &account_cache,
                tx,
                true,
                false,
            ) {
                // TODO: A transaction that is packed multiple times would also
                // throw an error here, but it should be normal.
                debug!("set tx err: tx={}, e={:?}", tx_hash, e);
            }
        }

        let (chain_id, best_height, best_block_number) = {
            (
                best_info.best_chain_id(),
                best_info.best_epoch_number,
                best_info.best_block_number,
            )
        };
        // FIXME: Needs further discussion here, some transactions may be valid
        // and invalid back and forth, does this matters?
        let vm_spec = self.machine.spec(best_block_number, best_height);
        let transitions = &self.machine.params().transition_heights;

        while let Some(tx) = recycle_tx_buffer.pop() {
            info!(
                "should not trigger recycle transaction, nonce = {}, sender = {:?}, \
                account nonce = {}, hash = {:?} .",
                &tx.nonce(), &tx.sender(),
                account_cache.get_nonce(&tx.sender())?, tx.hash);

            if let Err(e) = self.verify_transaction_tx_pool(
                &tx,
                /* basic_check = */ false,
                chain_id,
                best_height,
                transitions,
                &vm_spec,
            ) {
                warn!(
                    "Recycled transaction {:?} discarded due to not passing verification {}.",
                    tx.hash(), e
                );
            }
            if let Err(e) = self.add_transaction_with_readiness_check(
                inner,
                &account_cache,
                tx,
                false,
                true,
            ) {
                warn!("recycle tx err: e={:?}", e);
            }
        }
        debug!(
            "notify_new_best_info: {:?}",
            self.consensus_best_info.lock()
        );

        Ok(())
    }

    // For RPC use only
    pub fn get_best_info_with_parent_base_price(
        &self,
    ) -> (Arc<BestInformation>, Option<SpaceMap<U256>>) {
        let consensus_best_info_clone = self.consensus_best_info.lock().clone();
        debug!(
            "get_best_info_with_base_price: {:?}",
            consensus_best_info_clone
        );

        let params = self.machine.params();
        let parent_block = self
            .data_man
            .block_header_by_hash(&consensus_best_info_clone.best_block_hash)
            // The parent block must exists.
            .expect(&concat!(file!(), ":", line!(), ":", column!()));

        let pack_height = consensus_best_info_clone.best_epoch_number + 1;

        (
            consensus_best_info_clone,
            Some(parent_block.base_price().unwrap()),
        )
    }

    pub fn get_best_info_with_packed_transactions(
        &self, num_txs: usize, block_size_limit: usize,
        additional_transactions: Vec<Arc<SignedTransaction>>,
    ) -> (
        Arc<BestInformation>,
        U256,
        Vec<Arc<SignedTransaction>>,
        Option<SpaceMap<U256>>,
    ) {
        // We do not need to hold the lock because it is fine for us to generate
        // blocks that are slightly behind the best state.
        // We do not want to stall the consensus thread.
        let consensus_best_info_clone = self.consensus_best_info.lock().clone();
        debug!(
            "get_best_info_with_packed_transactions: {:?}",
            consensus_best_info_clone
        );

        let params = self.machine.params();

        let pack_height = consensus_best_info_clone.best_epoch_number + 1;

        let parent_block = self
            .data_man
            .block_header_by_hash(&consensus_best_info_clone.best_block_hash)
            // The parent block must exists.
            .expect(&concat!(file!(), ":", line!(), ":", column!()));
        let parent_block_gas_limit = *parent_block.gas_limit()
            * if 1 == pack_height {
                ELASTICITY_MULTIPLIER
            } else {
                1
            };

        let gas_limit_divisor = params.gas_limit_bound_divisor;
        let min_gas_limit = params.min_gas_limit;
        assert!(parent_block_gas_limit >= min_gas_limit);
        let gas_lower = max(
            parent_block_gas_limit - parent_block_gas_limit / gas_limit_divisor
                + 1,
            min_gas_limit,
        );
        let gas_upper = parent_block_gas_limit
            + parent_block_gas_limit / gas_limit_divisor
            - 1;

        let target_gas_limit =
            self.config.target_block_gas_limit * ELASTICITY_MULTIPLIER as u64;

        let self_gas_limit =
            min(max(target_gas_limit.into(), gas_lower), gas_upper);

        // TODO: remove this if, keep else branch
        let (transactions_from_pool, maybe_base_price) = if false {
            (vec![], None)
        } else {
            let parent_base_price = if pack_height <= 1 {
                params.init_base_price()
            } else {
                parent_block.base_price().unwrap()
            };

            let (txs, packing_base_price) = self.pack_transactions_1559(
                num_txs,
                self_gas_limit.clone(),
                parent_base_price,
                block_size_limit,
                consensus_best_info_clone.best_epoch_number,
                consensus_best_info_clone.best_block_number,
            );

            let mut base_price = packing_base_price;

            // May only happens in test mode
            if !additional_transactions.is_empty() {
                let iter = additional_transactions
                    .iter()
                    .chain(txs.iter())
                    .map(|x| &**x);
                match self.compute_1559_base_price(
                    &parent_block.hash(),
                    self_gas_limit,
                    iter,
                ) {
                    Ok(Some(p)) => {
                        base_price = p;
                    }
                    Ok(None) => {
                        warn!("Should not happen");
                    }
                    Err(e) => {
                        error!("Cannot compute base price with additinal transactions: {}", e);
                    }
                }
            }
            (txs, Some(base_price))
        };

        let transactions = [
            additional_transactions.as_slice(),
            transactions_from_pool.as_slice(),
        ]
        .concat();

        (
            consensus_best_info_clone,
            self_gas_limit,
            transactions,
            maybe_base_price,
        )
    }

    fn best_executed_state(
        data_man: &BlockDataManager, best_executed_epoch: StateIndex,
    ) -> StateDbResult<Arc<State>> {
        Ok(Arc::new(State::new(StateDb::new(
            data_man
                .storage_manager
                .get_state_no_commit(
                    best_executed_epoch,
                    /* try_open = */ false,
                    None,
                )?
                // Safe because the state is guaranteed to be available
                .unwrap(),
        ))?))
    }

    pub fn set_best_executed_epoch(
        &self, best_executed_epoch: StateIndex,
    ) -> StateDbResult<()> {
        *self.best_executed_state.lock() =
            Self::best_executed_state(&self.data_man, best_executed_epoch)?;

        Ok(())
    }

    fn get_best_state_account_cache(&self) -> AccountCache {
        let _timer = MeterTimer::time_func(TX_POOL_GET_STATE_TIMER.as_ref());
        AccountCache::new((&*self.best_executed_state.lock()).clone())
    }

    pub fn ready_for_mining(&self) -> bool {
        self.ready_for_mining.load(Ordering::SeqCst)
    }

    pub fn set_ready(&self) {
        self.ready_for_mining.store(true, Ordering::SeqCst);
    }
}
