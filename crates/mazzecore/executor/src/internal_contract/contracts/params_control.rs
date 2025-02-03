// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use mazze_parameters::internal_contract_addresses::PARAMS_CONTROL_CONTRACT_ADDRESS;
use mazze_types::{Address, U256};

use mazze_vm_interpreter::GasPriceTier;

use super::{super::impls::params_control::*, preludes::*};

make_solidity_contract! {
    pub struct ParamsControl(PARAMS_CONTROL_CONTRACT_ADDRESS, generate_fn_table, initialize: |_params: &CommonParams| 0, is_active: |_spec: &Spec| true);
}
fn generate_fn_table() -> SolFnTable {
    make_function_table!(
        CastVote,
        ReadVote,
        CurrentRound,
        TotalVotes,
        PosStakeForVotes
    )
}
group_impl_is_active!(
    |_spec: &Spec| true,
    CastVote,
    ReadVote,
    CurrentRound,
    TotalVotes
);
group_impl_is_active!(|_spec: &Spec| true, PosStakeForVotes,);

make_solidity_event! {
    pub struct VoteEvent("Vote(uint64,address,uint16,uint256[3])", indexed: (u64,Address,u16), non_indexed: [U256;3]);
}
make_solidity_event! {
    pub struct RevokeEvent("Revoke(uint64,address,uint16,uint256[3])", indexed: (u64,Address,u16), non_indexed: [U256;3]);
}

make_solidity_function! {
    struct CastVote((u64, Vec<Vote>), "castVote(uint64,(uint16,uint256[3])[])");
}
impl_function_type!(CastVote, "non_payable_write");

impl UpfrontPaymentTrait for CastVote {
    fn upfront_gas_payment(
        &self, (_, votes): &(u64, Vec<Vote>), _params: &ActionParams,
        context: &InternalRefContext,
    ) -> DbResult<U256> {
        let spec = context.spec;
        Ok(cast_vote_gas(votes.len(), spec).into())
    }
}

impl SimpleExecutionTrait for CastVote {
    fn execute_inner(
        &self, inputs: (u64, Vec<Vote>), params: &ActionParams,
        context: &mut InternalRefContext,
    ) -> vm::Result<()> {
        cast_vote(params.sender, inputs.0, inputs.1, params, context)
    }
}

make_solidity_function! {
    struct ReadVote(Address, "readVote(address)", Vec<Vote>);
}

impl_function_type!(ReadVote, "query", gas: |spec: &Spec| params_index_max(spec) * OPTION_INDEX_MAX * (spec.sload_gas + 2 * spec.sha3_gas));

impl SimpleExecutionTrait for ReadVote {
    fn execute_inner(
        &self, input: Address, params: &ActionParams,
        context: &mut InternalRefContext,
    ) -> vm::Result<Vec<Vote>> {
        read_vote(input, params, context)
    }
}

make_solidity_function! {
    struct CurrentRound((), "currentRound()", u64);
}
impl_function_type!(CurrentRound, "query", gas: |spec:&Spec| spec.tier_step_gas[(GasPriceTier::Low).idx()]);
impl SimpleExecutionTrait for CurrentRound {
    fn execute_inner(
        &self, _input: (), _params: &ActionParams,
        context: &mut InternalRefContext,
    ) -> vm::Result<u64> {
        Ok(context.env.number + 1)
    }
}

make_solidity_function! {
    struct TotalVotes(u64, "totalVotes(uint64)", Vec<Vote>);
}
impl_function_type!(TotalVotes, "query", gas: |spec: &Spec| params_index_max(spec) * OPTION_INDEX_MAX * spec.sload_gas);

impl SimpleExecutionTrait for TotalVotes {
    fn execute_inner(
        &self, input: u64, _params: &ActionParams,
        context: &mut InternalRefContext,
    ) -> vm::Result<Vec<Vote>> {
        total_votes(input, context)
    }
}

make_solidity_function! {
    struct PosStakeForVotes(u64, "posStakeForVotes(uint64)", U256);
}
impl_function_type!(PosStakeForVotes, "query", gas: |spec: &Spec| 2 * spec.sload_gas);

impl SimpleExecutionTrait for PosStakeForVotes {
    fn execute_inner(
        &self, input: u64, _params: &ActionParams,
        context: &mut InternalRefContext,
    ) -> vm::Result<U256> {
        pos_stake_for_votes(input, context)
    }
}

#[derive(Clone, Eq, PartialEq, Default)]
pub struct Vote {
    pub index: u16,
    pub votes: [U256; OPTION_INDEX_MAX],
}

impl solidity_abi::ABIVariable for Vote {
    const STATIC_LENGTH: Option<usize> = Some(32 * (1 + OPTION_INDEX_MAX));
    const BASIC_TYPE: bool = false;

    fn from_abi(data: &[u8]) -> Result<Self, solidity_abi::ABIDecodeError> {
        if data.len() < Self::STATIC_LENGTH.unwrap() {
            return Err(solidity_abi::ABIDecodeError("Invalid data length"));
        }

        let index = u16::from_be_bytes([data[0], data[1]]);
        let mut votes = [U256::zero(); OPTION_INDEX_MAX];

        for (i, vote) in votes.iter_mut().enumerate() {
            let start = 2 + i * 32;
            let end = start + 32;
            *vote = U256::from_big_endian(&data[start..end]);
        }

        Ok(Vote { index, votes })
    }

    fn to_abi(&self) -> solidity_abi::LinkedBytes {
        let mut result = Vec::new();
        result.extend_from_slice(&self.index.to_be_bytes());
        for vote in &self.votes {
            let mut bytes = [0u8; 32];
            vote.to_big_endian(&mut bytes);
            result.extend_from_slice(&bytes);
        }

        solidity_abi::LinkedBytes::from_bytes(result)
    }

    fn to_packed_abi(&self) -> solidity_abi::LinkedBytes {
        self.to_abi()
    }
}

pub const POW_BASE_REWARD_INDEX: u8 = 0;
pub const POS_REWARD_INTEREST_RATE_INDEX: u8 = 1;
pub const STORAGE_POINT_PROP_INDEX: u8 = 2;
pub const BASEFEE_PROP_INDEX: u8 = 3;
pub const PARAMETER_INDEX_MAX: usize = 4;

pub const OPTION_UNCHANGE_INDEX: u8 = 0;
pub const OPTION_INCREASE_INDEX: u8 = 1;
pub const OPTION_DECREASE_INDEX: u8 = 2;
pub const OPTION_INDEX_MAX: usize = 3;
