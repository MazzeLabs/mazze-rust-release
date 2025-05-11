// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

pub use system_storage_key::storage_point_prop;

mod system_storage_key {
    use mazze_parameters::internal_contract_addresses::PARAMS_CONTROL_CONTRACT_ADDRESS;
    use mazze_types::U256;

    use super::super::super::{
        components::storage_layout::*, contracts::system_storage::base_slot,
    };

    const STORAGE_POINT_PROP_SLOT: usize = 0;

    pub fn storage_point_prop() -> [u8; 32] {
        // Position of `storage_point_prop` (static slot)
        let base = base_slot(PARAMS_CONTROL_CONTRACT_ADDRESS)
            + U256::from(STORAGE_POINT_PROP_SLOT);
        u256_to_array(base)
    }
}
