// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

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

//! Environment information for transaction execution.

use std::collections::BTreeMap;

use mazze_types::{Address, Space, SpaceMap, H256, U256};
use primitives::BlockNumber;

/// Information concerning the execution environment for a
/// message-call/contract-creation.
#[derive(Debug, Clone, Default)]
pub struct Env {
    /// Chain ID
    pub chain_id: BTreeMap<Space, u32>,
    /// The block number.
    pub number: BlockNumber,
    /// The block author.
    pub author: Address,
    /// The block timestamp.
    pub timestamp: u64,
    /// The block difficulty.
    pub difficulty: U256,
    /// The block gas limit.
    pub gas_limit: U256,
    /// The last block hash.
    pub last_hash: H256,
    /// The total gas used in the block following execution of the transaction.
    pub accumulated_gas_used: U256,
    /// The epoch height.
    pub epoch_height: u64,
    /// The transaction_epoch_bound used to verify if a transaction has
    /// expired.
    pub transaction_epoch_bound: u64,
    /// Base gas price in CIP-1559, equals to 0 if CIP-1559 has not been
    /// activated
    pub base_gas_price: SpaceMap<U256>,
    /// Base gas price to miner according to in CIP-137
    pub burnt_gas_price: SpaceMap<U256>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_can_be_created_as_default() {
        let default_env = Env::default();

        assert_eq!(default_env.number, 0);
        assert_eq!(default_env.author, Address::default());
        assert_eq!(default_env.timestamp, 0);
        assert_eq!(default_env.difficulty, 0.into());
        assert_eq!(default_env.gas_limit, 0.into());
        assert_eq!(default_env.last_hash, H256::zero());
        assert_eq!(default_env.accumulated_gas_used, 0.into());
    }
}
