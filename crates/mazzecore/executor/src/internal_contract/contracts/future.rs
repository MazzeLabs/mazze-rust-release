use super::super::{components::SolFnTable, contracts::preludes::*};
use mazze_parameters::internal_contract_addresses::*;
use mazze_types::Address;
use mazze_vm_types::Spec;
use primitives::BlockNumber;

// Set the internal contract addresses to be activated in the future. So we can
// update the hardcoded test mode genesis state  without waiting for the
// implementation of each contract.
make_solidity_contract! {
    pub(super) struct Reserved3(RESERVED3, "placeholder");
}

make_solidity_contract! {
    pub(super) struct Reserved8(RESERVED8, "placeholder");
}
make_solidity_contract! {
    pub(super) struct Reserved9(RESERVED9, "placeholder");
}
make_solidity_contract! {
    pub(super) struct Reserved11(RESERVED11, "placeholder");
}
