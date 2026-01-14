// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use std::{
    collections::HashMap,
    env,
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
    sync::Arc,
};

use rustc_hex::FromHex;
use sha3_macro::keccak;
use solidity_abi::ABIEncodable;
use toml::Value;

use keylib::KeyPair;
use mazze_executor::internal_contract::initialize_internal_contract_accounts;
use mazze_internal_common::debug::ComputeEpochDebugRecord;
use mazze_parameters::{
    consensus::{GENESIS_GAS_LIMIT, ONE_MAZZE_IN_MAZZY},
    consensus_internal::GENESIS_TOKEN_COUNT_IN_MAZZE,
    genesis::*,
    internal_contract_addresses::SHIELDED_POOL_CONTRACT_ADDRESS,
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

// Native treasury address (type bits 0x1) derived from the genesis key.
const GENESIS_TREASURY_ADDRESS_HEX: &str =
    "0x1fd05dc5b53db270b52b4bc2b5068d41cef1b240";
const GENESIS_TREASURY_BALANCE_MAZZY_STR: &str =
    "2500000000000000000000000000";
const SHIELDED_POOL_GENESIS_FUND_MAZZE: u64 = 250_000_000;

fn genesis_treasury_address() -> Address {
    GENESIS_TREASURY_ADDRESS_HEX
        .trim_start_matches("0x")
        .parse::<Address>()
        .unwrap()
}

pub fn default(dev_or_test_mode: bool) -> HashMap<AddressWithSpace, U256> {
    let mut accounts: HashMap<AddressWithSpace, U256> = HashMap::new();
    if dev_or_test_mode {
        // FIXME: Decide the genesis initialization for mainnet.
        let balance = U256::from_dec_str("5000000000000000000000000000000000")
            .expect("Not overflow"); // 5*10^33
        accounts.insert(
            DEV_GENESIS_KEY_PAIR.address().with_native_space(),
            balance,
        );
        accounts.insert(
            DEV_GENESIS_KEY_PAIR_2.address().with_native_space(),
            balance,
        );
        accounts.insert(
            DEV_GENESIS_KEY_PAIR.evm_address().with_evm_space(),
            balance,
        );
        accounts.insert(
            DEV_GENESIS_KEY_PAIR_2.evm_address().with_evm_space(),
            balance,
        );
    }

    let genesis_address = genesis_treasury_address();
    let balance = U256::from_dec_str(GENESIS_TREASURY_BALANCE_MAZZY_STR)
        .expect("Not overflow"); // 2.5B
    accounts.insert(genesis_address.with_native_space(), balance);

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
    let treasury = genesis_treasury_address().with_native_space();
    let treasury_balance =
        U256::from_dec_str(GENESIS_TREASURY_BALANCE_MAZZY_STR).map_err(|e| {
            format!(
                "failed to parse treasury balance: value = {}, error = {:?}",
                GENESIS_TREASURY_BALANCE_MAZZY_STR, e
            )
        })?;
    accounts.entry(treasury).or_insert(treasury_balance);
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

    // Seed the shielded pool from the genesis treasury balance.
    let shielded_pool_seed = U256::from(SHIELDED_POOL_GENESIS_FUND_MAZZE)
        * U256::from(ONE_MAZZE_IN_MAZZY);
    if !shielded_pool_seed.is_zero() {
        let treasury = genesis_treasury_address().with_native_space();
        let pool = SHIELDED_POOL_CONTRACT_ADDRESS.with_native_space();
        if state.exists(&treasury).unwrap_or(false) {
            let treasury_balance = state.balance(&treasury).unwrap_or_default();
            if treasury_balance >= shielded_pool_seed {
                state
                    .transfer_balance(
                        &treasury,
                        &pool,
                        &shielded_pool_seed,
                        CleanupMode::NoEmpty,
                    )
                    .unwrap();
            } else {
                warn!(
                    "Genesis treasury balance {} < shielded pool seed {}; skipping seed",
                    treasury_balance, shielded_pool_seed
                );
            }
        } else {
            warn!("Genesis treasury account missing; skipping shielded pool seed");
        }
    }

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

    let mut genesis_transactions = vec![Arc::new(
        create_create2factory_transaction.fake_sign(genesis_account_address),
    )];

    let shielded_vk = load_shielded_vk_hex();
    if let Some(vk_bytes) = shielded_vk.as_ref() {
        let data = encode_set_verifying_key(vk_bytes);
        let mut set_vk_tx = NativeTransaction::default();
        set_vk_tx.nonce = U256::from(genesis_transactions.len());
        set_vk_tx.data = data.into();
        set_vk_tx.action = Action::Call(SHIELDED_POOL_CONTRACT_ADDRESS);
        set_vk_tx.chain_id = genesis_chain_id;
        set_vk_tx.gas = 5_000_000.into();
        set_vk_tx.gas_price = 1.into();
        set_vk_tx.storage_limit = 0;
        genesis_transactions
            .push(Arc::new(set_vk_tx.fake_sign(genesis_account_address)));
    }

    if need_to_execute {
        execute_genesis_transaction(
            genesis_transactions[0].as_ref(),
            &mut state,
            machine.clone(),
        );

        let (contract_address, _) = contract_address(
            CreateContractAddress::FromSenderNonceAndCodeHash,
            0,
            &genesis_account_address,
            &0.into(),
            genesis_transactions[0].as_ref().data(),
        );

        state
            .set_admin(&contract_address.address, &Address::zero())
            .expect("");
        info!("Genesis {:?} addresses: {:?}", "CREATE2FACTORY", contract_address);

        for tx in genesis_transactions.iter().skip(1) {
            execute_genesis_transaction(tx.as_ref(), &mut state, machine.clone());
        }

        if shielded_vk.is_some() {
            state
                .set_admin(
                    &SHIELDED_POOL_CONTRACT_ADDRESS,
                    &Address::zero(),
                )
                .expect("failed to clear shielded pool admin");
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

fn load_shielded_vk_hex() -> Option<Vec<u8>> {
    let mut candidates = Vec::new();
    if let Ok(path) = env::var("MAZZE_SHIELDED_VK_HEX") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            candidates.push(trimmed.to_string());
        }
    }
    candidates.push("run/shielded_vk.hex".to_string());
    candidates.push("shielded_vk.hex".to_string());

    for path in candidates {
        let path_ref = Path::new(&path);
        if !path_ref.exists() {
            continue;
        }
        let content = match std::fs::read_to_string(path_ref) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let hex = content.trim();
        if hex.is_empty() {
            continue;
        }
        let hex = hex.strip_prefix("0x").unwrap_or(hex);
        if let Ok(bytes) = hex.from_hex() {
            return Some(bytes);
        }
    }

    None
}

fn encode_set_verifying_key(vk: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(4 + vk.len() + 64);
    let selector = keccak!("setVerifyingKey(bytes)");
    data.extend_from_slice(&selector[0..4]);
    data.extend_from_slice(&vk.to_vec().abi_encode());
    data
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
