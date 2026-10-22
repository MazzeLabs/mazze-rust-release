// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! The shield-in bridge (spec §8.1). A shielded body carries no signature,
//! so it cannot debit an account: the depositor calls `shieldIn(bodyHash)`
//! from a signed native or eSpace transaction with the amount as its value.
//! From eSpace the call must be the transaction itself (a MetaMask send):
//! inside the EVM the address is code that reverts, see `mazze-eth-vm`. The value
//! lands in this contract's balance, which is the backing of the shielded
//! space, and a credit for `bodyHash` is recorded. The shielded body with
//! that hash consumes the credit when its slot executes.
//!
//! Shield-out needs no call: a shielded transaction's `v_public_out` is paid
//! from the backing to its destination when it executes.

use mazze_parameters::internal_contract_addresses::SHIELDED_BRIDGE_ADDRESS;
use mazze_types::{AddressSpaceUtil, Space, H256, U256};

use super::preludes::*;
use crate::{
    spec::SHIELDED_UNIT,
    executive_observer::AddressPocket, internal_bail, shielded_engine,
    substate::cleanup_mode,
};

make_solidity_contract! {
    pub struct ShieldedBridge(SHIELDED_BRIDGE_ADDRESS, generate_fn_table, initialize: |_params: &CommonParams| 0, is_active: |_spec: &Spec| true);
}

fn generate_fn_table() -> SolFnTable { make_function_table!(ShieldIn, Deposit) }

group_impl_is_active!(|_spec: &Spec| true, ShieldIn, Deposit);

make_solidity_function! {
    pub struct ShieldIn(H256, "shieldIn(bytes32)");
}

impl_function_type!(ShieldIn, "payable_write");

impl UpfrontPaymentTrait for ShieldIn {
    fn upfront_gas_payment(
        &self, _body_hash: &H256, _params: &ActionParams,
        context: &InternalRefContext,
    ) -> DbResult<U256> {
        Ok(U256::from(context.spec.sstore_reset_gas) * U256::from(2u64))
    }
}

impl SimpleExecutionTrait for ShieldIn {
    fn execute_inner(
        &self, body_hash: H256, params: &ActionParams,
        context: &mut InternalRefContext,
    ) -> vm::Result<()> {
        // Before activation no slot would ever consume the credit, and the
        // deposit would be locked for good.
        if !context.spec.shielded_active {
            internal_bail!("the shielded space is not active");
        }
        let value = params.value.value();
        if value.is_zero() {
            internal_bail!("shield-in requires a non-zero value");
        }
        // Shielded amounts are whole GMazzy (`SHIELDED_UNIT`), below 2^64
        // units: anything else is refused, never rounded.
        let unit = U256::from(SHIELDED_UNIT);
        if !(value % unit).is_zero() {
            internal_bail!("shield-in value must be a whole number of GMazzy");
        }
        if value / unit > U256::from(u64::MAX) {
            internal_bail!("shield-in value above 2^64 GMazzy");
        }
        // From eSpace the value landed on the bridge's eSpace account; the
        // backing is the native one, so it moves there.
        if params.space == Space::Ethereum {
            let espace = SHIELDED_BRIDGE_ADDRESS.with_evm_space();
            let backing = shielded_engine::backing_account();
            context.state.transfer_balance(
                &espace,
                &backing,
                &value,
                cleanup_mode(context.substate, context.spec),
            )?;
            context.tracer.trace_internal_transfer(
                AddressPocket::Balance(espace),
                AddressPocket::Balance(backing),
                value,
            );
            // The value left eSpace; a shield-out to eSpace adds it back.
            context.state.sub_total_evm_tokens(value);
        }
        shielded_engine::record_deposit(
            context.state,
            body_hash.as_bytes(),
            value,
            context.env.epoch_height,
        )?;
        Ok(())
    }
}

make_solidity_function! {
    pub struct Deposit(H256, "deposit(bytes32)", U256);
}

impl_function_type!(Deposit, "query", gas: |spec: &Spec| spec.sload_gas);

impl SimpleExecutionTrait for Deposit {
    fn execute_inner(
        &self, body_hash: H256, _params: &ActionParams,
        context: &mut InternalRefContext,
    ) -> vm::Result<U256> {
        Ok(shielded_engine::deposit_credit(context.state, body_hash.as_bytes())?)
    }
}
