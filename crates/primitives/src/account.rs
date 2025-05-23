// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::{bytes::Bytes, hash::KECCAK_EMPTY};
use mazze_types::{
    address_util::AddressUtil, Address, AddressSpaceUtil, AddressWithSpace,
    Space, H256, U256,
};
use rlp::{Decodable, DecoderError, Encodable, Rlp, RlpStream};
use rlp_derive::{RlpDecodable, RlpEncodable};

use std::{fmt, sync::Arc};

#[derive(Debug, PartialEq, Clone)]
pub enum AddressSpace {
    Builtin,
    User,
    Contract,
}

#[derive(Debug, PartialEq, Clone)]
pub enum AccountError {
    ReservedAddressSpace(Address),
    AddressSpaceMismatch(Address, AddressSpace),
    InvalidRlp(DecoderError),
}

#[derive(Clone, Debug, Ord, PartialOrd, Eq, PartialEq)]
pub struct CodeInfo {
    pub code: Arc<Bytes>,
    pub owner: Address,
}

impl CodeInfo {
    #[inline]
    pub fn code_size(&self) -> usize {
        self.code.len()
    }
}

impl Encodable for CodeInfo {
    fn rlp_append(&self, stream: &mut RlpStream) {
        stream.begin_list(2).append(&*self.code).append(&self.owner);
    }
}

impl Decodable for CodeInfo {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        Ok(Self {
            code: Arc::new(rlp.val_at(0)?),
            owner: rlp.val_at(1)?,
        })
    }
}

#[derive(
    Clone,
    Debug,
    Ord,
    PartialOrd,
    Eq,
    PartialEq,
    Default,
    RlpDecodable,
    RlpEncodable,
)]
pub struct StoragePoints {
    pub unused: U256,
    pub used: U256,
}

#[derive(Clone, Debug, Ord, PartialOrd, Eq, PartialEq, Default)]
pub struct SponsorInfo {
    /// This is the address of the sponsor for gas cost of the contract.
    pub sponsor_for_gas: Address,
    /// This is the address of the sponsor for collateral of the contract.
    pub sponsor_for_collateral: Address,
    /// This is the upper bound of sponsor gas cost per tx.
    pub sponsor_gas_bound: U256,
    /// This is the amount of tokens sponsor for gas cost to the contract.
    pub sponsor_balance_for_gas: U256,
    /// This is the amount of tokens sponsor for collateral to the contract.
    pub sponsor_balance_for_collateral: U256,
    /// This is the storage point used for the contract (obtained from the
    /// sponsor balance for collateral).
    pub storage_points: Option<StoragePoints>,
}

impl SponsorInfo {
    pub fn unused_storage_points(&self) -> U256 {
        self.storage_points
            .as_ref()
            .map_or(U256::zero(), |x| x.unused)
    }
}

impl Encodable for SponsorInfo {
    fn rlp_append(&self, s: &mut RlpStream) {
        match &self.storage_points {
            None => {
                s.begin_list(5);
                s.append(&self.sponsor_for_gas);
                s.append(&self.sponsor_for_collateral);
                s.append(&self.sponsor_gas_bound);
                s.append(&self.sponsor_balance_for_gas);
                s.append(&self.sponsor_balance_for_collateral);
            }
            Some(points) => {
                s.begin_list(6);
                s.append(&self.sponsor_for_gas);
                s.append(&self.sponsor_for_collateral);
                s.append(&self.sponsor_gas_bound);
                s.append(&self.sponsor_balance_for_gas);
                s.append(&self.sponsor_balance_for_collateral);
                s.append(points);
            }
        }
    }
}

impl Decodable for SponsorInfo {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        match rlp.item_count()? {
            5 => Ok(SponsorInfo {
                sponsor_for_gas: rlp.val_at(0)?,
                sponsor_for_collateral: rlp.val_at(1)?,
                sponsor_gas_bound: rlp.val_at(2)?,
                sponsor_balance_for_gas: rlp.val_at(3)?,
                sponsor_balance_for_collateral: rlp.val_at(4)?,
                storage_points: None,
            }),
            6 => Ok(SponsorInfo {
                sponsor_for_gas: rlp.val_at(0)?,
                sponsor_for_collateral: rlp.val_at(1)?,
                sponsor_gas_bound: rlp.val_at(2)?,
                sponsor_balance_for_gas: rlp.val_at(3)?,
                sponsor_balance_for_collateral: rlp.val_at(4)?,
                storage_points: Some(rlp.val_at(5)?),
            }),
            _ => Err(DecoderError::RlpInvalidLength),
        }
    }
}

#[derive(Clone, Debug, Ord, PartialOrd, Eq, PartialEq)]
pub struct Account {
    /// This field is not part of Account data, but kept for convenience. It
    /// should be rarely used except for debugging.
    address_local_info: AddressWithSpace,
    pub balance: U256,
    pub nonce: U256,
    pub code_hash: H256,
    // /// This is the number of tokens used in staking.
    // pub staking_balance: U256,
    /// This is the number of tokens used as collateral for storage, which will
    /// be returned to balance if the storage is released.
    pub collateral_for_storage: U256,
    /// This is the accumulated interest return.
    // pub accumulated_interest_return: U256,
    /// This is the address of the administrator of the contract.
    pub admin: Address,
    /// This is the sponsor information of the contract.
    pub sponsor_info: SponsorInfo,
}

/// Defined for Rlp serialization/deserialization.
#[derive(RlpEncodable, RlpDecodable)]
pub struct BasicAccount {
    dummy_diferentiator: u8,
    pub balance: U256,
    pub nonce: U256,
    // /// This is the number of tokens used in staking.
    // pub staking_balance: U256,
    /// This is the number of tokens used as collateral for storage, which will
    /// be returned to balance if the storage is released.
    pub collateral_for_storage: U256,
    // /// This is the accumulated interest return.
    // pub accumulated_interest_return: U256,
}

/// Defined for Rlp serialization/deserialization.
#[derive(RlpEncodable, RlpDecodable)]
pub struct ContractAccount {
    pub balance: U256,
    pub nonce: U256,
    pub code_hash: H256,
    // /// This is the number of tokens used in staking.
    // pub staking_balance: U256,
    /// This is the number of tokens used as collateral for storage, which will
    /// be returned to balance if the storage is released.
    pub collateral_for_storage: U256,
    // /// This is the accumulated interest return.
    // pub accumulated_interest_return: U256,
    /// This is the address of the administrator of the contract.
    pub admin: Address,
    /// This is the sponsor information of the contract.
    pub sponsor_info: SponsorInfo,
}

#[derive(RlpEncodable, RlpDecodable)]
pub struct EthereumAccount {
    pub balance: U256,
    pub nonce: U256,
    pub code_hash: H256,
}

impl Account {
    pub fn address(&self) -> &AddressWithSpace {
        &self.address_local_info
    }

    pub fn set_address(&mut self, address: AddressWithSpace) {
        self.address_local_info = address;
    }

    pub fn new_empty(address: &AddressWithSpace) -> Account {
        Self::new_empty_with_balance(address, &U256::from(0), &U256::from(0))
    }

    pub fn new_empty_with_balance(
        address: &AddressWithSpace, balance: &U256, nonce: &U256,
    ) -> Account {
        Self {
            address_local_info: *address,
            balance: *balance,
            nonce: *nonce,
            code_hash: KECCAK_EMPTY,
            // staking_balance: 0.into(),
            collateral_for_storage: 0.into(),
            // accumulated_interest_return: 0.into(),
            admin: Address::zero(),
            sponsor_info: Default::default(),
        }
    }

    fn from_basic_account(address: Address, a: BasicAccount) -> Self {
        Self {
            address_local_info: address.with_native_space(),
            balance: a.balance,
            nonce: a.nonce,
            code_hash: KECCAK_EMPTY,
            // staking_balance: a.staking_balance,
            collateral_for_storage: a.collateral_for_storage,
            // accumulated_interest_return: a.accumulated_interest_return,
            admin: Address::zero(),
            sponsor_info: Default::default(),
        }
    }

    pub fn from_contract_account(address: Address, a: ContractAccount) -> Self {
        Self {
            address_local_info: address.with_native_space(),
            balance: a.balance,
            nonce: a.nonce,
            code_hash: a.code_hash,
            // staking_balance: a.staking_balance,
            collateral_for_storage: a.collateral_for_storage,
            // accumulated_interest_return: a.accumulated_interest_return,
            admin: a.admin,
            sponsor_info: a.sponsor_info,
        }
    }

    fn from_ethereum_account(address: Address, a: EthereumAccount) -> Self {
        let address = address.with_evm_space();
        Self {
            address_local_info: address,
            balance: a.balance,
            nonce: a.nonce,
            code_hash: a.code_hash,
            ..Self::new_empty(&address)
        }
    }

    pub fn to_basic_account(&self) -> BasicAccount {
        assert_eq!(self.address_local_info.space, Space::Native);
        BasicAccount {
            dummy_diferentiator: 1,
            balance: self.balance,
            nonce: self.nonce,
            // staking_balance: self.staking_balance,
            collateral_for_storage: self.collateral_for_storage,
            // accumulated_interest_return: self.accumulated_interest_return,
        }
    }

    pub fn to_contract_account(&self) -> ContractAccount {
        assert_eq!(self.address_local_info.space, Space::Native);
        ContractAccount {
            balance: self.balance,
            nonce: self.nonce,
            code_hash: self.code_hash,
            // staking_balance: self.staking_balance,
            collateral_for_storage: self.collateral_for_storage,
            // accumulated_interest_return: self.accumulated_interest_return,
            admin: self.admin,
            sponsor_info: self.sponsor_info.clone(),
        }
    }

    pub fn to_evm_account(&self) -> EthereumAccount {
        assert_eq!(self.address_local_info.space, Space::Ethereum);
        // assert!(self.staking_balance.is_zero());
        assert!(self.collateral_for_storage.is_zero());
        // assert!(self.accumulated_interest_return.is_zero());
        assert!(self.admin.is_zero());
        assert_eq!(self.sponsor_info, Default::default());
        EthereumAccount {
            balance: self.balance,
            nonce: self.nonce,
            code_hash: self.code_hash,
        }
    }

    pub fn new_from_rlp(
        address: Address, rlp: &Rlp,
    ) -> Result<Self, AccountError> {
        let account = match rlp.item_count()? {
            6 => Self::from_contract_account(
                address,
                ContractAccount::decode(rlp)?,
            ),
            // TODO: fix this; both ethereum and native accounts have 3 fields
            4 => Self::from_basic_account(address, BasicAccount::decode(rlp)?),
            3 => Self::from_ethereum_account(
                address,
                EthereumAccount::decode(rlp)?,
            ),
            _ => {
                return Err(AccountError::InvalidRlp(
                    DecoderError::RlpIncorrectListLen,
                ));
            }
        };
        Ok(account)
    }
}

impl Encodable for Account {
    fn rlp_append(&self, stream: &mut RlpStream) {
        if self.address_local_info.space == Space::Ethereum {
            stream.append_internal(&self.to_evm_account());
            return;
        }

        // After CIP-80, an address started by 0x8 is still stored as
        // contract format in underlying db, even if it may be a normal address.
        // In order to achieve backward compatible.
        //
        // It is impossible to have an all-zero hash value. But some previous
        // bug make one of the genesis accounts has all zero genesis hash.
        if self.code_hash != KECCAK_EMPTY && !self.code_hash.is_zero()
            || self.address_local_info.address.is_contract_address()
        {
            // A contract address can hold balance before its initialization
            // as a recipient of a simple transaction.
            // So we always determine how to serialize by the address type bits.
            stream.append_internal(&self.to_contract_account());
        } else {
            stream.append_internal(&self.to_basic_account());
        }
    }
}

impl From<DecoderError> for AccountError {
    fn from(err: DecoderError) -> Self {
        AccountError::InvalidRlp(err)
    }
}

impl fmt::Display for AccountError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let msg = match self {
            AccountError::ReservedAddressSpace(address) => {
                format!("Address space is reserved for {:?}", address)
            }
            AccountError::AddressSpaceMismatch(address, address_space) => {
                format!(
                    "Address {:?} not in address space {:?}",
                    address, address_space
                )
            }
            AccountError::InvalidRlp(err) => {
                format!("Transaction has invalid RLP structure: {}.", err)
            }
        };

        f.write_fmt(format_args!("Account error ({})", msg))
    }
}

impl std::error::Error for AccountError {
    fn description(&self) -> &str {
        "Account error"
    }
}

#[cfg(test)]
fn test_random_account(
    type_bit: Option<u8>, non_empty_hash: bool, contract_type: bool,
) {
    let mut address = Address::random();
    address.set_address_type_bits(type_bit.unwrap_or(0x40));

    let admin = Address::random();
    let sponsor_info = SponsorInfo {
        sponsor_for_gas: Address::random(),
        sponsor_for_collateral: Address::random(),
        sponsor_balance_for_gas: U256::from(123),
        sponsor_balance_for_collateral: U256::from(124),
        sponsor_gas_bound: U256::from(2),
        storage_points: None,
    };

    let code_hash = if non_empty_hash {
        H256::random()
    } else {
        KECCAK_EMPTY
    };

    let account = if contract_type {
        Account::from_contract_account(
            address,
            ContractAccount {
                balance: 1000.into(),
                nonce: 123.into(),
                code_hash,
                // staking_balance: 10000000.into(),
                collateral_for_storage: 23.into(),
                // accumulated_interest_return: 456.into(),
                admin,
                sponsor_info,
            },
        )
    } else {
        Account::from_basic_account(
            address,
            BasicAccount {
                dummy_diferentiator: 1,
                balance: 1000.into(),
                nonce: 123.into(),
                // staking_balance: 10000000.into(),
                collateral_for_storage: 23.into(),
                // accumulated_interest_return: 456.into(),
            },
        )
    };
    assert_eq!(
        account,
        Account::new_from_rlp(
            account.address_local_info.address,
            &Rlp::new(&account.rlp_bytes()),
        )
        .unwrap()
    );
}

#[test]
fn test_account_serde() {
    // Original normal address
    test_random_account(Some(0x10), false, false);
    // Original contract address
    test_random_account(Some(0x80), true, true);
    // Uninitialized contract address && new normal address
    test_random_account(Some(0x80), false, true);

    // New normal address
    test_random_account(None, false, false);
    test_random_account(Some(0x80), false, false);

    test_random_account(None, true, true);
    test_random_account(Some(0x80), true, true);
}
