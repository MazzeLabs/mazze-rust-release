// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

pub mod constants;
pub mod resources;

pub use constants::*;
pub use resources::*;

use move_core_types::account_address::AccountAddress;

pub fn main_chain_select_address() -> AccountAddress {
    AccountAddress::from_hex_literal("0x1D9")
        .expect("Parsing valid hex literal should always succeed")
}

pub fn election_select_address() -> AccountAddress {
    AccountAddress::from_hex_literal("0x1DA")
        .expect("Parsing valid hex literal should always succeed")
}

pub fn retire_address() -> AccountAddress {
    AccountAddress::from_hex_literal("0x1DB")
        .expect("Parsing valid hex literal should always succeed")
}

pub fn unlock_address() -> AccountAddress {
    AccountAddress::from_hex_literal("0x1DC")
        .expect("Parsing valid hex literal should always succeed")
}

pub fn register_address() -> AccountAddress {
    AccountAddress::from_hex_literal("0x1DD")
        .expect("Parsing valid hex literal should always succeed")
}

pub fn update_voting_power_address() -> AccountAddress {
    AccountAddress::from_hex_literal("0x1DE")
        .expect("Parsing valid hex literal should always succeed")
}

pub fn dispute_address() -> AccountAddress {
    AccountAddress::from_hex_literal("0x1DF")
        .expect("Parsing valid hex literal should always succeed")
}

pub fn reward_distribution_address() -> AccountAddress {
    AccountAddress::from_hex_literal("0x1E0")
        .expect("Parsing valid hex literal should always succeed")
}
