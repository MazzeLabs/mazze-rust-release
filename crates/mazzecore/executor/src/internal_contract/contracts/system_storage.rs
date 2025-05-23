// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use super::preludes::*;
use mazze_parameters::internal_contract_addresses::SYSTEM_STORAGE_ADDRESS;
use mazze_types::U256;

make_solidity_contract! {
    pub struct SystemStorage(SYSTEM_STORAGE_ADDRESS, SolFnTable::default, initialize: |_params: &CommonParams| 0, is_active: |_spec: &Spec| true);
}

pub fn base_slot(contract: Address) -> U256 {
    let hash = keccak(H256::from(contract).as_ref());
    U256::from_big_endian(hash.as_ref())
}
