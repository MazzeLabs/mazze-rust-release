// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use std::convert::TryInto;

use lazy_static::lazy_static;
use mazze_math::power_two_fractional;
use mazze_parameters::consensus_internal::DAO_MIN_VOTE_PERCENTAGE;
use mazze_statedb::Result as DbResult;
use mazze_types::{Address, U256, U512};
use mazze_vm_types::{self as vm, ActionParams, Spec};

use super::super::{
    components::{InternalRefContext, SolidityEventTrait},
    contracts::params_control::*,
};
use crate::{internal_bail, state::State};

pub use system_storage_key::storage_point_prop;

pub fn params_index_max(spec: &Spec) -> usize {
    let mut max = PARAMETER_INDEX_MAX;
    if !spec.cip1559 {
        max -= 1;
    }
    if !spec.cip107 {
        max -= 1;
    }
    max
}

/// Solidity variable sequences.
/// ```solidity
/// struct VoteStats {
///     uint[3] pow_base_reward dynamic,
///     uint[3] pos_interest_rate dynamic,
/// }
/// VoteStats current_votes dynamic;
/// VoteStats settled_votes dynamic;
/// uint current_pos_staking;
/// uint settled_pos_staking;
/// ```
mod system_storage_key {
    use mazze_parameters::internal_contract_addresses::PARAMS_CONTROL_CONTRACT_ADDRESS;
    use mazze_types::U256;

    use super::super::super::{
        components::storage_layout::*, contracts::system_storage::base_slot,
    };

    const CURRENT_VOTES_SLOT: usize = 0;
    const SETTLED_VOTES_SLOT: usize = 1;
    const CURRENT_POS_STAKING_SLOT: usize = 2;
    const SETTLED_POS_STAKING_SLOT: usize = 3;
    const STORAGE_POINT_PROP_SLOT: usize = 4;

    pub fn storage_point_prop() -> [u8; 32] {
        // Position of `storage_point_prop` (static slot)
        let base = base_slot(PARAMS_CONTROL_CONTRACT_ADDRESS)
            + U256::from(STORAGE_POINT_PROP_SLOT);
        u256_to_array(base)
    }
}
