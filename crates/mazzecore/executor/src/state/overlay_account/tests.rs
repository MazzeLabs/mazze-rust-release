// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use super::OverlayAccount;
use keccak_hash::KECCAK_EMPTY;
use mazze_statedb::StateDb;
use mazze_storage::{
    tests::new_state_manager_for_unit_test, StorageManagerTrait,
};
use mazze_types::{address_util::AddressUtil, Address, AddressSpaceUtil, U256};
use primitives::{
    account::ContractAccount, storage::STORAGE_LAYOUT_REGULAR_V0, Account,
    SponsorInfo,
};

use crate::state::get_state_for_genesis_write;
use primitives::is_default::IsDefault;
use std::str::FromStr;

fn test_account_is_default(account: &mut OverlayAccount) {
    let storage_manager = new_state_manager_for_unit_test();
    let state = get_state_for_genesis_write(&storage_manager);

    assert!(account.as_account().is_default());
}

#[test]
fn new_overlay_account_is_default() {
    let normal_addr =
        Address::from_str("1000000000000000000000000000000000000000")
            .unwrap()
            .with_native_space();
    let builtin_addr =
        Address::from_str("0000000000000000000000000000000000000000")
            .unwrap()
            .with_native_space();

    test_account_is_default(&mut OverlayAccount::new_basic(
        &normal_addr,
        U256::zero(),
    ));
    test_account_is_default(&mut OverlayAccount::new_basic(
        &builtin_addr,
        U256::zero(),
    ));
}

#[test]
fn test_overlay_account_create() {
    let mut address = Address::random();
    address.set_user_account_type_bits();
    let address_with_space = address.with_native_space();
    let account = Account::new_empty_with_balance(
        &address_with_space,
        &U256::zero(),
        &U256::zero(),
    );
    // test new from account 1
    let overlay_account =
        OverlayAccount::from_loaded(&address_with_space, account);
    assert_eq!(overlay_account.address().address, address);
    assert_eq!(*overlay_account.balance(), 0.into());
    assert_eq!(*overlay_account.nonce(), 0.into());
    assert_eq!(overlay_account.collateral_for_storage(), 0.into());
    assert_eq!(overlay_account.code_hash(), KECCAK_EMPTY);
    assert_eq!(overlay_account.is_newly_created_contract(), false);
    assert_eq!(*overlay_account.admin(), Address::zero());
    assert_eq!(*overlay_account.sponsor_info(), Default::default());

    let mut contract_addr = Address::random();
    contract_addr.set_contract_type_bits();
    let contract_addr_with_space = contract_addr.with_native_space();
    let mut user_addr = Address::random();
    user_addr.set_user_account_type_bits();
    let user_addr_with_space = user_addr.with_native_space();
    let admin = Address::random();
    let sponsor_info = SponsorInfo {
        sponsor_for_gas: Address::random(),
        sponsor_for_collateral: Address::random(),
        sponsor_balance_for_gas: U256::from(123),
        sponsor_balance_for_collateral: U256::from(124),
        sponsor_gas_bound: U256::from(2),
        storage_points: None,
    };
    let account = Account::from_contract_account(
        contract_addr,
        ContractAccount {
            balance: 101.into(),
            nonce: 55.into(),
            code_hash: KECCAK_EMPTY,
            collateral_for_storage: 455.into(),
            admin,
            sponsor_info: sponsor_info.clone(),
        },
    );

    // test new from account 2
    let overlay_account =
        OverlayAccount::from_loaded(&contract_addr_with_space, account);
    assert_eq!(overlay_account.address().address, contract_addr);
    assert_eq!(*overlay_account.balance(), 101.into());
    assert_eq!(*overlay_account.nonce(), 55.into());
    assert_eq!(overlay_account.collateral_for_storage(), 455.into());
    assert_eq!(overlay_account.code_hash(), KECCAK_EMPTY);
    assert_eq!(overlay_account.is_newly_created_contract(), false);
    assert_eq!(*overlay_account.admin(), admin);
    assert_eq!(*overlay_account.sponsor_info(), sponsor_info);

    // test new basic
    let overlay_account =
        OverlayAccount::new_basic(&user_addr_with_space, 1011.into());
    assert_eq!(overlay_account.address().address, user_addr);
    assert_eq!(*overlay_account.balance(), 1011.into());
    assert_eq!(overlay_account.collateral_for_storage(), 0.into());
    assert_eq!(overlay_account.code_hash(), KECCAK_EMPTY);
    assert_eq!(overlay_account.is_newly_created_contract(), false);
    assert_eq!(overlay_account.is_contract(), false);
    assert_eq!(overlay_account.is_basic(), true);
    assert_eq!(*overlay_account.admin(), Address::zero());
    assert_eq!(*overlay_account.sponsor_info(), Default::default());

    // test new contract
    let mut overlay_account = OverlayAccount::new_contract(
        &contract_addr,
        5678.into(),
        false,
        Some(STORAGE_LAYOUT_REGULAR_V0),
    );
    assert_eq!(overlay_account.address().address, contract_addr);
    assert_eq!(*overlay_account.balance(), 5678.into());
    assert_eq!(overlay_account.collateral_for_storage(), 0.into());
    assert_eq!(overlay_account.code_hash(), KECCAK_EMPTY);
    assert_eq!(overlay_account.is_newly_created_contract(), true);
    assert_eq!(overlay_account.is_contract(), true);
    assert_eq!(
        overlay_account.storage_layout_change(),
        Some(&STORAGE_LAYOUT_REGULAR_V0)
    );
    assert_eq!(*overlay_account.admin(), Address::zero());
    assert_eq!(*overlay_account.sponsor_info(), Default::default());
    overlay_account.inc_nonce();

    // test new contract with admin
    let overlay_account = OverlayAccount::new_contract_with_admin(
        &contract_addr_with_space,
        5678.into(),
        &admin,
        false,
        Some(STORAGE_LAYOUT_REGULAR_V0),
        false,
    );
    assert_eq!(overlay_account.address().address, contract_addr);
    assert_eq!(*overlay_account.balance(), 5678.into());
    assert_eq!(overlay_account.collateral_for_storage(), 0.into());
    assert_eq!(overlay_account.code_hash(), KECCAK_EMPTY);
    assert_eq!(overlay_account.is_newly_created_contract(), true);
    assert_eq!(overlay_account.is_contract(), true);
    assert_eq!(
        overlay_account.storage_layout_change(),
        Some(&STORAGE_LAYOUT_REGULAR_V0)
    );
    assert_eq!(*overlay_account.admin(), admin);
    assert_eq!(*overlay_account.sponsor_info(), Default::default());
}

fn check_ordered_feature(vote_stake_list: &VoteStakeList) {
    for i in 1..vote_stake_list.len() {
        assert!(
            vote_stake_list[i - 1].unlock_block_number
                < vote_stake_list[i].unlock_block_number
        );
        assert!(vote_stake_list[i - 1].amount > vote_stake_list[i].amount);
    }
}

fn init_test_account() -> OverlayAccount {
    let storage_manager = new_state_manager_for_unit_test();
    let db = StateDb::new(storage_manager.get_state_for_genesis_write());
    let mut address = Address::random();
    address.set_user_account_type_bits();
    let address_with_space = address.with_native_space();
    let account = Account::new_empty_with_balance(
        &address_with_space,
        &10_000_000.into(),
        &U256::zero(),
    );

    let mut overlay_account =
        OverlayAccount::from_loaded(&address_with_space, account.clone());
    overlay_account.cache_ext_fields(&db).unwrap();

    overlay_account
}

#[test]
fn test_clone_overwrite() {
    let mut address = Address::random();
    address.set_contract_type_bits();
    let address_with_space = address.with_native_space();
    let admin = Address::random();
    let sponsor_info = SponsorInfo {
        sponsor_for_gas: Address::random(),
        sponsor_for_collateral: Address::random(),
        sponsor_balance_for_gas: U256::from(123),
        sponsor_balance_for_collateral: U256::from(124),
        sponsor_gas_bound: U256::from(2),
        storage_points: None,
    };
    let account1 = Account::from_contract_account(
        address,
        ContractAccount {
            balance: 1000.into(),
            nonce: 123.into(),
            code_hash: KECCAK_EMPTY,
            collateral_for_storage: 23.into(),
            admin,
            sponsor_info,
        },
    );

    let admin = Address::random();
    let sponsor_info = SponsorInfo {
        sponsor_for_gas: Address::random(),
        sponsor_for_collateral: Address::random(),
        sponsor_balance_for_gas: U256::from(1233),
        sponsor_balance_for_collateral: U256::from(1244),
        sponsor_gas_bound: U256::from(23),
        storage_points: None,
    };
    let account2 = Account::from_contract_account(
        address,
        ContractAccount {
            balance: 1001.into(),
            nonce: 124.into(),
            code_hash: KECCAK_EMPTY,
            collateral_for_storage: 24.into(),
            admin,
            sponsor_info,
        },
    );

    let mut overlay_account1 =
        OverlayAccount::from_loaded(&address_with_space, account1.clone());
    let mut overlay_account2 =
        OverlayAccount::from_loaded(&address_with_space, account2.clone());
    assert_eq!(account1, overlay_account1.as_account());
    assert_eq!(account2, overlay_account2.as_account());

    overlay_account1.set_storage_simple(vec![0; 32], U256::zero());
    assert_eq!(account1, overlay_account1.as_account());
    assert_eq!(overlay_account1.storage_write_cache.len(), 1);
    let overlay_account = overlay_account1.clone_account();
    assert_eq!(account1, overlay_account.as_account());
    assert_eq!(overlay_account.storage_write_cache.len(), 1);

    overlay_account2.set_storage_simple(vec![0; 32], U256::zero());
    overlay_account2.set_storage_simple(vec![1; 32], U256::zero());
    overlay_account1 = overlay_account2;
    assert_ne!(account1, overlay_account1.as_account());
    assert_eq!(account2, overlay_account1.as_account());
    assert_eq!(overlay_account1.storage_write_cache.len(), 2);
}
