// Copyright 2015-2018 Parity Technologies (UK) Ltd.
// This file is part of Parity.

// Parity is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity.  If not, see <http://www.gnu.org/licenses/>.

// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Evm input params.
use super::call_create_type::{CallType, CreateType};
use mazze_bytes::Bytes;
use mazze_types::{Address, Space, H256, U256};
use std::sync::Arc;

/// Transaction value
#[derive(Clone, Debug)]
pub enum ActionValue {
    /// Value that should be transferred
    Transfer(U256),
    /// Apparent value for transaction (not transferred)
    Apparent(U256),
}

/// Type of the way parameters encoded
#[derive(Clone, Debug)]
pub enum ParamsType {
    /// Parameters are included in code
    Embedded,
    /// Parameters are passed in data section
    Separate,
}

impl ActionValue {
    /// Returns action value as U256.
    pub fn value(&self) -> U256 {
        match *self {
            ActionValue::Transfer(x) | ActionValue::Apparent(x) => x,
        }
    }

    /// Returns the transfer action value of the U256-convertable raw value
    pub fn transfer<T: Into<U256>>(transfer_value: T) -> ActionValue {
        ActionValue::Transfer(transfer_value.into())
    }

    /// Returns the apparent action value of the U256-convertable raw value
    pub fn apparent<T: Into<U256>>(apparent_value: T) -> ActionValue {
        ActionValue::Apparent(apparent_value.into())
    }
}

/// Action (call/create) input params. Everything else should be specified in
/// Externalities.
#[derive(Clone, Debug)]
pub struct ActionParams {
    /// Space
    pub space: Space,
    /// Address of currently executed code.
    pub code_address: Address,
    /// Hash of currently executed code.
    pub code_hash: H256,
    /// Receive address. Usually equal to code_address,
    /// except when called using CALLCODE.
    pub address: Address,
    /// Sender of current part of the transaction.
    pub sender: Address,
    /// This is the address of original sender of the transaction.
    pub original_sender: Address,
    /// This is the address of account who will pay collateral for storage in
    /// the whole execution.
    pub storage_owner: Address,
    /// Gas paid up front for transaction execution
    pub gas: U256,
    /// Gas price.
    pub gas_price: U256,
    /// Transaction value.
    pub value: ActionValue,
    /// Code being executed.
    pub code: Option<Arc<Bytes>>,
    /// Input data.
    pub data: Option<Bytes>,
    /// Type of call
    pub call_type: CallType,
    /// Type of create
    pub create_type: CreateType,
    /// Param types encoding
    pub params_type: ParamsType,
}

#[cfg(any(test, feature = "testonly_code"))]
impl Default for ActionParams {
    /// Returns default ActionParams initialized with zeros
    fn default() -> ActionParams {
        use keccak_hash::KECCAK_EMPTY;
        ActionParams {
            space: Space::Native,
            code_address: Address::default(),
            code_hash: KECCAK_EMPTY,
            address: Address::default(),
            sender: Address::default(),
            original_sender: Address::default(),
            storage_owner: Address::default(),
            gas: U256::zero(),
            gas_price: U256::zero(),
            value: ActionValue::Transfer(U256::zero()),
            code: None,
            data: None,
            call_type: CallType::Call,
            create_type: CreateType::None,
            params_type: ParamsType::Separate,
        }
    }
}
