// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use mazze_parameters::internal_contract_addresses::CONTEXT_CONTRACT_ADDRESS;
use mazze_types::{Address, BigEndianHash, U256};
use mazze_vm_interpreter::GasPriceTier;

use crate::{internal_contract::epoch_hash_slot, return_if};

use super::preludes::*;

make_solidity_contract! {
    pub struct Context(CONTEXT_CONTRACT_ADDRESS, generate_fn_table, initialize: |_params: &CommonParams| 0, is_active: |_spec: &Spec| true);
}

fn generate_fn_table() -> SolFnTable {
    make_function_table!(EpochNumber, EpochHash)
}

group_impl_is_active!(|_spec: &Spec| true, EpochNumber,);

group_impl_is_active!(|spec: &Spec| spec.cip133_core, EpochHash);

make_solidity_function! {
    struct EpochNumber((), "epochNumber()", U256);
}

// same gas cost as the `NUMBER` opcode
impl_function_type!(EpochNumber, "query", gas: |spec: &Spec| spec.tier_step_gas[(GasPriceTier::Base).idx()]);

impl SimpleExecutionTrait for EpochNumber {
    fn execute_inner(
        &self, _input: (), _params: &ActionParams,
        context: &mut InternalRefContext,
    ) -> vm::Result<U256> {
        Ok(U256::from(context.env.epoch_height))
    }
}

make_solidity_function! {
    struct EpochHash(U256, "epochHash(uint256)", H256);
}

impl_function_type!(EpochHash, "query", gas: |spec: &Spec| spec.sload_gas);

impl SimpleExecutionTrait for EpochHash {
    fn execute_inner(
        &self, number: U256, _params: &ActionParams,
        context: &mut InternalRefContext,
    ) -> vm::Result<H256> {
        return_if!(number > U256::from(u64::MAX));

        let number = number.as_u64();

        return_if!(number < context.spec.cip133_e);
        return_if!(number > context.env.epoch_height);
        return_if!(number
            .checked_add(65536)
            .map_or(false, |n| n <= context.env.epoch_height));
        let res = context.state.get_system_storage(&epoch_hash_slot(number))?;
        Ok(BigEndianHash::from_uint(&res))
    }
}

#[test]
fn test_context_contract_sig() {
    check_func_signature!(EpochNumber, "f4145a83");
}
