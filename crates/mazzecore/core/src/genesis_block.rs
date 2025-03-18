// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader, Read},
    sync::Arc,
};

use rustc_hex::FromHex;
use toml::Value;

use keylib::KeyPair;
use mazze_executor::internal_contract::initialize_internal_contract_accounts;
use mazze_internal_common::debug::ComputeEpochDebugRecord;
use mazze_parameters::{
    consensus::{GENESIS_GAS_LIMIT, ONE_MAZZE_IN_MAZZY},
    consensus_internal::GENESIS_TOKEN_COUNT_IN_MAZZE,
    genesis::*,
};
use mazze_statedb::StateDb;
use mazze_storage::{StorageManager, StorageManagerTrait};
use mazze_types::{
    address_util::AddressUtil, Address, AddressSpaceUtil, AddressWithSpace,
    Space, U256,
};
use primitives::{
    Action, Block, BlockHeaderBuilder, BlockReceipts, SignedTransaction,
};
use secret_store::SecretStore;

use crate::verification::{compute_receipts_root, compute_transaction_root};
use mazze_executor::{
    executive::{
        contract_address, ExecutionOutcome, ExecutiveContext, TransactOptions,
    },
    machine::Machine,
    state::{CleanupMode, State},
};
use mazze_vm_types::{CreateContractAddress, Env};
use primitives::transaction::native_transaction::NativeTransaction;

pub fn default(dev_or_test_mode: bool) -> HashMap<AddressWithSpace, U256> {
    if !dev_or_test_mode {
        return HashMap::new();
    }
    let mut accounts: HashMap<AddressWithSpace, U256> = HashMap::new();
    // FIXME: Decide the genesis initialization for mainnet.
    let balance = U256::from_dec_str("5000000000000000000000000000000000")
        .expect("Not overflow"); // 5*10^33
    accounts
        .insert(DEV_GENESIS_KEY_PAIR.address().with_native_space(), balance);
    accounts.insert(
        DEV_GENESIS_KEY_PAIR_2.address().with_native_space(),
        balance,
    );
    accounts
        .insert(DEV_GENESIS_KEY_PAIR.evm_address().with_evm_space(), balance);
    accounts.insert(
        DEV_GENESIS_KEY_PAIR_2.evm_address().with_evm_space(),
        balance,
    );
    accounts
}

pub fn load_secrets_file(
    path: &String, secret_store: &SecretStore,
) -> Result<HashMap<AddressWithSpace, U256>, String> {
    let file = File::open(path)
        .map_err(|e| format!("failed to open file: {:?}", e))?;
    let buffered = BufReader::new(file);

    let mut accounts: HashMap<AddressWithSpace, U256> = HashMap::new();
    let balance =
        U256::from_dec_str("10000000000000000000000").map_err(|e| {
            format!(
                "failed to parse balance: value = {}, error = {:?}",
                "10000000000000000000000", e
            )
        })?;
    for line in buffered.lines() {
        let keypair =
            KeyPair::from_secret(line.unwrap().parse().unwrap()).unwrap();
        accounts.insert(keypair.address().with_native_space(), balance.clone());
        secret_store.insert(keypair);
    }
    Ok(accounts)
}

/// ` test_net_version` is used to update the genesis author so that after
/// resetting, the chain of the older version will be discarded
pub fn genesis_block(
    storage_manager: &Arc<StorageManager>,
    genesis_accounts: HashMap<AddressWithSpace, U256>,
    test_net_version: Address, initial_difficulty: U256, machine: Arc<Machine>,
    need_to_execute: bool, genesis_chain_id: Option<u32>,
) -> Block {
    let mut state =
        State::new(StateDb::new(storage_manager.get_state_for_genesis_write()))
            .expect("Failed to initialize state");

    let mut genesis_block_author = test_net_version;
    genesis_block_author.set_user_account_type_bits();

    initialize_internal_contract_accounts(
        &mut state,
        machine.internal_contracts().initialized_at_genesis(),
    )
    .expect("no db error");
    trace!("genesis_accounts: {:?}", genesis_accounts);
    for (addr, balance) in genesis_accounts {
        state
            .add_balance(&addr, &balance, CleanupMode::NoEmpty)
            .unwrap();
        state.add_total_issued(balance);
        if addr.space == Space::Ethereum {
            state.add_total_evm_tokens(balance);
        }
    }
    let genesis_account_address = GENESIS_ACCOUNT_ADDRESS.with_native_space();

    let genesis_token_count = U256::from(GENESIS_TOKEN_COUNT_IN_MAZZE)
        * U256::from(ONE_MAZZE_IN_MAZZY);
    state.add_total_issued(genesis_token_count);

    let genesis_account_init_balance =
        U256::from(ONE_MAZZE_IN_MAZZY) * 100 + genesis_token_count;
    state
        .add_balance(
            &genesis_account_address,
            &genesis_account_init_balance,
            CleanupMode::NoEmpty,
        )
        .unwrap();

    let mut debug_record = Some(ComputeEpochDebugRecord::default());

    let genesis_chain_id = genesis_chain_id.unwrap_or(0);
    let mut genesis_transaction = NativeTransaction::default();
    genesis_transaction.data = GENESIS_TRANSACTION_DATA_STR.as_bytes().into();
    genesis_transaction.action = Action::Call(Default::default());
    genesis_transaction.chain_id = genesis_chain_id;

    let mut create_create2factory_transaction = NativeTransaction::default();
    create_create2factory_transaction.nonce = 0.into();
    create_create2factory_transaction.data =
        GENESIS_TRANSACTION_CREATE_CREATE2FACTORY
            .from_hex()
            .unwrap();
    create_create2factory_transaction.action = Action::Create;
    create_create2factory_transaction.chain_id = genesis_chain_id;
    create_create2factory_transaction.gas = 3000000.into();
    create_create2factory_transaction.gas_price = 1.into();
    create_create2factory_transaction.storage_limit = 512;

    let genesis_transactions = vec![Arc::new(
        create_create2factory_transaction.fake_sign(genesis_account_address),
    )];

    if need_to_execute {
        const CREATE2FACTORY_TX_INDEX: usize = 1;
        let contract_name_list = vec!["CREATE2FACTORY"];

        for i in CREATE2FACTORY_TX_INDEX..=contract_name_list.len() {
            execute_genesis_transaction(
                genesis_transactions[i - 1].as_ref(),
                &mut state,
                machine.clone(),
            );

            let (contract_address, _) = contract_address(
                CreateContractAddress::FromSenderNonceAndCodeHash,
                0,
                &genesis_account_address,
                &(i - 1).into(),
                genesis_transactions[i - 1].as_ref().data(),
            );

            state
                .set_admin(&contract_address.address, &Address::zero())
                .expect("");
            info!(
                "Genesis {:?} addresses: {:?}",
                contract_name_list[i - 1],
                contract_address
            );
        }
    }

    state
        .genesis_special_remove_account(&genesis_account_address.address)
        .expect("Clean account failed");

    let state_root = state
        .compute_state_root_for_genesis(
            /* debug_record = */ debug_record.as_mut(),
        )
        .unwrap();
    let receipt_root = compute_receipts_root(&vec![Arc::new(BlockReceipts {
        receipts: vec![],
        block_number: 0,
        secondary_reward: U256::zero(),
        tx_execution_error_messages: vec![],
    })]);

    let mut genesis = Block::new(
        BlockHeaderBuilder::new()
            .with_deferred_state_root(state_root.aux_info.state_root_hash)
            .with_deferred_receipts_root(receipt_root)
            .with_gas_limit(GENESIS_GAS_LIMIT.into())
            .with_author(genesis_block_author)
            .with_difficulty(initial_difficulty)
            .with_transactions_root(compute_transaction_root(
                &genesis_transactions,
            ))
            .build(),
        genesis_transactions,
    );
    genesis.block_header.compute_hash();
    debug!(
        "Initialize genesis_block={:?} hash={:?}",
        genesis,
        genesis.hash()
    );

    state
        .set_initial_storage_point_prop()
        .expect("Failed to initialize storage point prop");

    state
        .commit(
            genesis.block_header.hash(),
            /* debug_record = */ debug_record.as_mut(),
        )
        .unwrap();
    genesis.block_header.pow_hash = Some(Default::default());
    debug!(
        "genesis debug_record {}",
        serde_json::to_string(&debug_record).unwrap()
    );

    genesis
}

fn execute_genesis_transaction(
    transaction: &SignedTransaction, state: &mut State, machine: Arc<Machine>,
) {
    let env = Env::default();

    let options = TransactOptions::default();
    let r = {
        ExecutiveContext::new(
            state,
            &env,
            machine.as_ref(),
            &machine.spec(env.number, env.epoch_height),
        )
        .transact(transaction, options)
        .unwrap()
    };

    match &r {
        ExecutionOutcome::Finished(_executed) => {}
        _ => {
            panic!("genesis transaction should not fail! err={:?}", r);
        }
    }
}

pub fn load_file(
    path: &String, address_parser: impl Fn(&str) -> Result<Address, String>,
) -> Result<HashMap<AddressWithSpace, U256>, String> {
    let mut content = String::new();
    let mut file = File::open(path)
        .map_err(|e| format!("failed to open file: {:?}", e))?;
    file.read_to_string(&mut content)
        .map_err(|e| format!("failed to read file content: {:?}", e))?;
    let account_values = content
        .parse::<toml::Value>()
        .map_err(|e| format!("failed to parse toml file: {:?}", e))?;

    let mut accounts: HashMap<AddressWithSpace, U256> = HashMap::new();
    match account_values {
        Value::Table(table) => {
            for (key, value) in table {
                let addr = address_parser(&key).map_err(|e| {
                    format!(
                        "failed to parse address: value = {}, error = {:?}",
                        key, e
                    )
                })?;

                match value {
                    Value::String(balance) => {
                        let balance = U256::from_dec_str(&balance).map_err(|e| format!("failed to parse balance: value = {}, error = {:?}", balance, e))?;
                        accounts.insert(addr.with_native_space(), balance);
                    }
                    _ => {
                        return Err(
                            "balance in toml file requires String type".into(),
                        );
                    }
                }
            }
        }
        _ => {
            return Err(format!(
                "invalid root value type {:?} in toml file",
                account_values.type_str()
            ));
        }
    }

    Ok(accounts)
}
