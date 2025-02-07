// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use mazze_parameters::internal_contract_addresses::PARAMS_CONTROL_CONTRACT_ADDRESS;
use mazze_types::{Address, U256};

use mazze_vm_interpreter::GasPriceTier;

use super::{super::impls::params_control::*, preludes::*};

pub const POW_BASE_REWARD_INDEX: u8 = 0;
pub const POS_REWARD_INTEREST_RATE_INDEX: u8 = 1;
pub const STORAGE_POINT_PROP_INDEX: u8 = 2;
pub const BASEFEE_PROP_INDEX: u8 = 3;
pub const PARAMETER_INDEX_MAX: usize = 4;

pub const OPTION_UNCHANGE_INDEX: u8 = 0;
pub const OPTION_INCREASE_INDEX: u8 = 1;
pub const OPTION_DECREASE_INDEX: u8 = 2;
pub const OPTION_INDEX_MAX: usize = 3;
