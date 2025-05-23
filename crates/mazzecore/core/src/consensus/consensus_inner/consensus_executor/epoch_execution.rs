use super::ConsensusExecutionHandler;
use std::{convert::From, sync::Arc};

use alloy_rpc_types_trace::geth::GethDebugTracingOptions;
use geth_tracer::{GethTraceWithHash, GethTracer, TxExecContext};

use mazze_statedb::Result as DbResult;
use mazze_types::{Space, SpaceMap, H256, U256};
use primitives::{
    receipt::BlockReceipts, Action, Block, BlockNumber, EpochId, Receipt,
    SignedTransaction, TransactionIndex,
};

use crate::{
    block_data_manager::BlockDataManager,
    consensus::consensus_inner::consensus_executor::GOOD_TPS_METER,
    state_prefetcher::{prefetch_accounts, PrefetchTaskHandle},
};
use mazze_execute_helper::{
    exec_tracer::TransactionExecTraces,
    observer::Observer,
    tx_outcome::{make_process_tx_outcome, ProcessTxOutcome},
};
use mazze_executor::{
    executive::{ExecutiveContext, TransactOptions, TransactSettings},
    internal_contract::{
        block_hash_slot, epoch_hash_slot, initialize_internal_contract_accounts,
    },
    state::State,
};
use mazze_vm_types::Env;

pub enum VirtualCall<'a> {
    GethTrace(GethTask<'a>),
}

pub struct GethTask<'a> {
    pub(super) tx_hash: Option<H256>,
    pub(super) opts: GethDebugTracingOptions,
    pub(super) answer: &'a mut Vec<GethTraceWithHash>,
}

impl ConsensusExecutionHandler {
    pub(super) fn process_epoch_transactions<'a>(
        &self, epoch_id: EpochId, state: &mut State,
        epoch_blocks: &Vec<Arc<Block>>, start_block_number: u64,
        on_local_main: bool, virtual_call: Option<VirtualCall<'a>>,
    ) -> DbResult<Vec<Arc<BlockReceipts>>> {
        self.prefetch_storage_for_execution(epoch_id, state, epoch_blocks);

        let main_block = epoch_blocks.last().expect("Epoch not empty");

        let dry_run = virtual_call.is_some();

        self.before_epoch_execution(state, &*main_block)?;

        let base_gas_price =
            main_block.block_header.base_price().unwrap_or_default();

        let burnt_gas_price =
            base_gas_price.map_all(|x| state.burnt_gas_price(x));
        let context = EpochProcessContext {
            on_local_main,
            executive_trace: self.config.executive_trace,
            dry_run,
            virtual_call,
            main_block,
            base_gas_price,
            burnt_gas_price,
        };

        let mut epoch_recorder = EpochProcessRecorder::new();

        let mut block_context = BlockProcessContext::first_block(
            &context,
            epoch_blocks.first().unwrap(),
            start_block_number,
        );

        for (idx, block) in epoch_blocks.iter().enumerate() {
            if idx > 0 {
                block_context.next_block(block);
            }

            self.process_block_transactions(
                &block_context,
                state,
                &mut epoch_recorder,
            )?;
        }

        if let Some(VirtualCall::GethTrace(task)) = context.virtual_call {
            std::mem::swap(&mut epoch_recorder.geth_traces, task.answer);
        }

        if !dry_run && on_local_main {
            self.tx_pool.recycle_transactions(epoch_recorder.repack_tx);
        }

        debug!("Finish processing tx for epoch");
        Ok(epoch_recorder.receipts)
    }

    fn prefetch_storage_for_execution(
        &self, epoch_id: EpochId, state: &mut State,
        epoch_blocks: &Vec<Arc<Block>>,
    ) {
        // Prefetch accounts for transactions.
        // The return value _prefetch_join_handles is used to join all threads
        // before the exit of this function.
        let prefetch_join_handles = match self
            .execution_state_prefetcher
            .as_ref()
        {
            Some(prefetcher) => {
                let mut accounts = vec![];
                for block in epoch_blocks.iter() {
                    for transaction in block.transactions.iter() {
                        accounts.push(&transaction.sender);
                        match transaction.action() {
                            Action::Call(ref address) => accounts.push(address),
                            _ => {}
                        }
                    }
                }

                prefetch_accounts(prefetcher, epoch_id, state, accounts)
            }
            None => PrefetchTaskHandle {
                task_epoch_id: epoch_id,
                state,
                prefetcher: None,
                accounts: vec![],
            },
        };

        // TODO:
        //   Make the state shared ref for vm execution, then remove this drop.
        //   When the state can be made shared, prefetch can happen at the same
        //   time of the execution, the vm execution do not have to wait
        //   for prefetching to finish.
        prefetch_join_handles.wait_for_task();
        drop(prefetch_join_handles);
    }

    fn make_block_env(&self, block_context: &BlockProcessContext) -> Env {
        let BlockProcessContext {
            epoch_context:
                &EpochProcessContext {
                    main_block,
                    base_gas_price,
                    burnt_gas_price,
                    ..
                },
            block,
            block_number,
            last_hash,
        } = *block_context;

        let epoch_height = main_block.block_header.height();
        let chain_id = self.machine.params().chain_id_map(epoch_height);
        Env {
            chain_id,
            number: block_number,
            author: block.block_header.author().clone(),
            timestamp: main_block.block_header.timestamp(),
            difficulty: block.block_header.difficulty().clone(),
            accumulated_gas_used: U256::zero(),
            last_hash,
            gas_limit: U256::from(block.block_header.gas_limit()),
            epoch_height,
            transaction_epoch_bound: self
                .verification_config
                .transaction_epoch_bound,
            base_gas_price,
            burnt_gas_price,
        }
    }

    fn process_block_transactions(
        &self, block_context: &BlockProcessContext, state: &mut State,
        epoch_recorder: &mut EpochProcessRecorder,
    ) -> DbResult<()> {
        let BlockProcessContext {
            epoch_context: &EpochProcessContext { on_local_main, .. },
            block,
            block_number,
            ..
        } = *block_context;

        debug!(
            "process txs in block: hash={:?}, tx count={:?}",
            block.hash(),
            block.transactions.len()
        );

        // TODO: ideally, this function should not have return value.
        // However, the previous implementation read `secondary_reward` in an
        // intermediate step. Since we are not sure which steps will influnce
        // `secondary_reward`, we must `secondary_reward` at the same point to
        // keep the backward compatible.
        let secondary_reward =
            self.before_block_execution(state, block_number, block)?;

        let mut env = self.make_block_env(block_context);

        let mut block_recorder =
            BlockProcessRecorder::new(epoch_recorder.evm_tx_idx);

        for (idx, transaction) in block.transactions.iter().enumerate() {
            self.process_transaction(
                idx,
                transaction,
                block_context,
                state,
                &mut env,
                on_local_main,
                &mut block_recorder,
            )?;
        }

        block_recorder.finish_block(
            &self.data_man,
            epoch_recorder,
            block_context,
            secondary_reward,
        );

        Ok(())
    }

    fn process_transaction(
        &self, idx: usize, transaction: &Arc<SignedTransaction>,
        block_context: &BlockProcessContext, state: &mut State, env: &mut Env,
        on_local_main: bool, recorder: &mut BlockProcessRecorder,
    ) -> DbResult<()> {
        let rpc_index = recorder.tx_idx[transaction.space()];

        let block = &block_context.block;
        let dry_run = block_context.epoch_context.dry_run;

        let machine = self.machine.as_ref();

        let spec = machine.spec(env.number, env.epoch_height);

        let options = TransactOptions {
            observer: self.make_observer(transaction, block_context),
            settings: TransactSettings::all_checks(),
        };

        let execution_outcome =
            ExecutiveContext::new(state, env, machine, &spec)
                .transact(transaction, options)?;
        execution_outcome.log(transaction, &block_context.block.hash());

        if let Some(burnt_fee) = execution_outcome
            .try_as_executed()
            .and_then(|e| e.burnt_fee)
        {
            state.burn_by_mip1559(burnt_fee);
        };

        let r = make_process_tx_outcome(
            execution_outcome,
            &mut env.accumulated_gas_used,
            transaction.hash,
            &spec,
        );

        if r.receipt.tx_success() {
            GOOD_TPS_METER.mark(1);
        }

        let tx_skipped = r.receipt.tx_skipped();
        let phantom_txs = r.phantom_txs.clone();

        recorder.receive_tx_outcome(r, transaction, block_context);

        if !on_local_main || tx_skipped || dry_run {
            // Skip transaction index persist
            return Ok(());
        }

        let hash = transaction.hash();

        self.data_man.insert_transaction_index(
            &hash,
            &TransactionIndex {
                block_hash: block.hash(),
                real_index: idx,
                is_phantom: false,
                rpc_index: Some(rpc_index),
            },
        );

        // persist tx index for phantom transactions.
        // note: in some cases, main chain reorgs will result in
        // different phantom txs (with different hashes) for the
        // same Mazze space tx. we do not remove invalidated
        // hashes here, but leave it up to the RPC layer to handle
        // this instead.
        let evm_chain_id = env.chain_id[&Space::Ethereum];
        let evm_tx_index = &mut recorder.tx_idx[Space::Ethereum];

        for ptx in phantom_txs {
            self.data_man.insert_transaction_index(
                &ptx.into_eip155(evm_chain_id).hash(),
                &TransactionIndex {
                    block_hash: block.hash(),
                    real_index: idx,
                    is_phantom: true,
                    rpc_index: Some(*evm_tx_index),
                },
            );

            *evm_tx_index += 1;
        }

        Ok(())
    }

    fn make_observer(
        &self, transaction: &Arc<SignedTransaction>,
        block_context: &BlockProcessContext,
    ) -> Observer {
        use alloy_rpc_types_trace::geth::{
            GethDebugBuiltInTracerType::*, GethDebugTracerType::BuiltInTracer,
        };

        let mut observer = if self.config.executive_trace {
            Observer::with_tracing()
        } else {
            Observer::with_no_tracing()
        };

        if let Some(VirtualCall::GethTrace(ref task)) =
            block_context.epoch_context.virtual_call
        {
            let need_trace =
                task.tx_hash.map_or(true, |hash| transaction.hash() == hash);
            let support_tracer = matches!(
                task.opts.tracer,
                Some(BuiltInTracer(
                    FourByteTracer | CallTracer | PreStateTracer | NoopTracer
                )) | None
            );
            let tx_gas_limit = transaction.gas_limit().as_u64();

            if need_trace && support_tracer {
                observer.geth_tracer = Some(GethTracer::new(
                    TxExecContext {
                        tx_gas_limit,
                        block_height: block_context
                            .epoch_context
                            .main_block
                            .block_header
                            .height(),
                        block_number: block_context.block_number,
                    },
                    Arc::clone(&self.machine),
                    task.opts.clone(),
                ))
            }
        }
        observer
    }

    fn before_epoch_execution(
        &self, state: &mut State, main_block: &Block,
    ) -> DbResult<()> {
        let epoch_number = main_block.block_header.height();
        let hash = main_block.hash();

        state.set_system_storage(
            epoch_hash_slot(epoch_number).into(),
            U256::from_big_endian(&hash.0),
        )?;

        Ok(())
    }

    pub fn before_block_execution(
        &self, state: &mut State, block_number: BlockNumber, block: &Block,
    ) -> DbResult<U256> {
        state.set_system_storage(
            block_hash_slot(block_number).into(),
            U256::from_big_endian(&block.hash().0),
        )?;

        initialize_internal_contract_accounts(
            state,
            self.machine
                .internal_contracts()
                .initialized_at(block_number),
        )?;

        Ok(U256::zero())
    }
}

struct EpochProcessContext<'a> {
    on_local_main: bool,
    executive_trace: bool,
    virtual_call: Option<VirtualCall<'a>>,
    dry_run: bool,

    main_block: &'a Block,

    base_gas_price: SpaceMap<U256>,
    burnt_gas_price: SpaceMap<U256>,
}

struct BlockProcessContext<'a, 'b> {
    epoch_context: &'b EpochProcessContext<'a>,
    block: &'b Block,
    block_number: u64,
    last_hash: H256,
}

impl<'a, 'b> BlockProcessContext<'a, 'b> {
    fn first_block(
        epoch_context: &'b EpochProcessContext<'a>, block: &'b Block,
        start_block_number: u64,
    ) -> Self {
        let EpochProcessContext { main_block, .. } = *epoch_context;
        let last_hash = *main_block.block_header.parent_hash();
        Self {
            epoch_context,
            block,
            block_number: start_block_number,
            last_hash,
        }
    }

    fn next_block(&mut self, block: &'b Block) {
        self.last_hash = self.block.hash();
        self.block_number += 1;
        self.block = block;
    }
}

#[derive(Default)]
struct EpochProcessRecorder {
    receipts: Vec<Arc<BlockReceipts>>,
    // staking_events: Vec<StakingEvent>,
    repack_tx: Vec<Arc<SignedTransaction>>,
    geth_traces: Vec<GethTraceWithHash>,

    evm_tx_idx: usize,
}

impl EpochProcessRecorder {
    fn new() -> Self {
        Default::default()
    }
}

struct BlockProcessRecorder {
    receipt: Vec<Receipt>,
    tx_error_msg: Vec<String>,
    traces: Vec<TransactionExecTraces>,
    geth_traces: Vec<GethTraceWithHash>,
    repack_tx: Vec<Arc<SignedTransaction>>,
    // staking_events: Vec<StakingEvent>,
    tx_idx: SpaceMap<usize>,
}

impl BlockProcessRecorder {
    fn new(evm_tx_idx: usize) -> BlockProcessRecorder {
        let mut tx_idx = SpaceMap::default();
        tx_idx[Space::Ethereum] = evm_tx_idx;
        Self {
            receipt: vec![],
            tx_error_msg: vec![],
            traces: vec![],
            geth_traces: vec![],
            repack_tx: vec![],
            // staking_events: vec![],
            tx_idx,
        }
    }

    fn receive_tx_outcome(
        &mut self, r: ProcessTxOutcome, tx: &Arc<SignedTransaction>,
        block_context: &BlockProcessContext,
    ) {
        let EpochProcessContext {
            on_local_main,
            executive_trace,
            ..
        } = *block_context.epoch_context;

        if on_local_main && r.consider_repacked {
            self.repack_tx.push(tx.clone())
        }

        let not_skipped = !r.receipt.tx_skipped();

        if executive_trace {
            self.traces.push(r.tx_traces.into());
        }

        self.receipt.push(r.receipt);
        self.tx_error_msg.push(r.tx_exec_error_msg);

        if let Some(trace) = r.geth_trace {
            self.geth_traces.push(GethTraceWithHash {
                trace,
                tx_hash: tx.hash(),
                space: tx.space(),
            });
        }

        match tx.space() {
            Space::Native => {
                self.tx_idx[Space::Native] += 1;
            }
            Space::Ethereum if not_skipped => {
                self.tx_idx[Space::Ethereum] += 1;
            }
            _ => {}
        };
    }

    fn finish_block(
        self, data_man: &BlockDataManager,
        epoch_recorder: &mut EpochProcessRecorder,
        block_context: &BlockProcessContext, secondary_reward: U256,
    ) {
        let BlockProcessContext {
            epoch_context:
                &EpochProcessContext {
                    on_local_main,
                    executive_trace,
                    main_block,
                    dry_run,
                    ..
                },
            block,
            block_number,
            ..
        } = *block_context;

        let block_receipts = Arc::new(BlockReceipts {
            receipts: self.receipt,
            // An existing bug makes the block_number is one larger than the
            // actual.
            block_number: block_number + 1,
            secondary_reward,
            tx_execution_error_messages: self.tx_error_msg,
        });

        epoch_recorder.receipts.push(block_receipts.clone());
        // epoch_recorder.staking_events.extend(self.staking_events);
        epoch_recorder.repack_tx.extend(self.repack_tx);
        epoch_recorder.geth_traces.extend(self.geth_traces);

        epoch_recorder.evm_tx_idx = self.tx_idx[Space::Ethereum];

        if dry_run {
            return;
        }

        if executive_trace {
            data_man.insert_block_traces(
                block.hash(),
                self.traces.into(),
                main_block.hash(),
                on_local_main,
            );
        }

        data_man.insert_block_execution_result(
            block.hash(),
            main_block.hash(),
            block_receipts.clone(),
            on_local_main,
        );
    }
}
