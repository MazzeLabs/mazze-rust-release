use super::State;
use mazze_parameters::{
    consensus::ONE_MAZZE_IN_MAZZY, consensus_internal::CIP137_BASEFEE_PROP_INIT,
};
use mazze_types::U256;

impl State {
    /// Calculate the secondary reward for the next block number.
    pub fn bump_block_number_accumulate_interest(&mut self) {
        // assert!(self.checkpoints.get_mut().is_empty());
        // let interset_rate_per_block = self.global_stat.get::<InterestRate>();
        // let accumulate_interest_rate =
        //     self.global_stat.val::<AccumulateInterestRate>();
        // *accumulate_interest_rate = *o
        //     * (*INTEREST_RATE_PER_BLOCK_SCALE + interset_rate_per_block)
        //     / *INTEREST_RATE_PER_BLOCK_SCALE;
    }

    pub fn secondary_reward(&self) -> U256 {
        // assert!(self.checkpoints.read().is_empty());
        // let secondary_reward = *self.global_stat.refr::<TotalStorage>()
        //     * *self.global_stat.refr::<InterestRate>()
        //     / *INTEREST_RATE_PER_BLOCK_SCALE;
        // // TODO: the interest from tokens other than storage and staking should
        // // send to public fund.
        // secondary_reward
        U256::zero()
    }

    pub fn burnt_gas_price(&self, base_price: U256) -> U256 {
        if base_price.is_zero() {
            return U256::zero();
        }
        // let prop = self.get_base_price_prop();
        // base_price - base_price * prop / (U256::from(ONE_MAZZE_IN_MAZZY) + prop)
        base_price
    }
}

/// Initialize CIP-137 for the whole system.
pub fn initialize_cip137(state: &mut State) {
    debug!("set base_fee_prop to {}", CIP137_BASEFEE_PROP_INIT);
    // state.set_base_fee_prop(CIP137_BASEFEE_PROP_INIT.into());
}
