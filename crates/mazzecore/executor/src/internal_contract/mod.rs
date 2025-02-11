// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

mod components;
mod contracts;
mod impls;
mod utils;

pub use self::{
    components::{
        InterfaceTrait, InternalContractExec, InternalContractMap,
        InternalContractTrait, InternalRefContext, SolidityEventTrait,
    },
    contracts::{
        cross_space::{
            events as cross_space_events, is_call_create_sig, is_withdraw_sig,
        },
        initialize_internal_contract_accounts,
    },
    impls::{
        admin::suicide,
        context::{block_hash_slot, epoch_hash_slot},
        cross_space::{evm_map, Resume},
    },
};
