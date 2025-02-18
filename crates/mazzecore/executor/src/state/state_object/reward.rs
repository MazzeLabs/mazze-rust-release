use super::State;
use mazze_parameters::{
    consensus::ONE_MAZZE_IN_MAZZY, consensus_internal::CIP137_BASEFEE_PROP_INIT,
};
use mazze_types::U256;

use mazze_parameters::staking::INTEREST_RATE_PER_BLOCK_SCALE;
use mazze_statedb::global_params::*;

impl State {
    pub fn burn_by_cip1559(&mut self, by: U256) {
        // This function is called after transaction exeuction. At this time,
        // the paid transaction fee has already been in the core space.
        *self.global_stat.val::<TotalBurnt1559>() += by;
        self.sub_total_issued(by);
    }

    pub fn get_base_price_prop(&self) -> U256 {
        self.global_stat.get::<BaseFeeProp>()
    }

    pub fn set_base_fee_prop(&mut self, val: U256) {
        *self.global_stat.val::<BaseFeeProp>() = val;
    }

    pub fn burnt_gas_price(&self, base_price: U256) -> U256 {
        if base_price.is_zero() {
            return U256::zero();
        }
        let prop = self.get_base_price_prop();
        base_price - base_price * prop / (U256::from(ONE_MAZZE_IN_MAZZY) + prop)
    }
}

/// Initialize CIP-137 for the whole system.
pub fn initialize_cip137(state: &mut State) {
    debug!("set base_fee_prop to {}", CIP137_BASEFEE_PROP_INIT);
    state.set_base_fee_prop(CIP137_BASEFEE_PROP_INIT.into());
}
