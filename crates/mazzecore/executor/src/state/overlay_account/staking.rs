use mazze_types::U256;

use super::OverlayAccount;

impl OverlayAccount {
    pub fn vote_lock(&mut self, amount: U256, unlock_block_number: u64) {
        self.address.assert_native();
        assert!(self.vote_stake_list.is_some());
        // assert!(amount <= self.staking_balance);
        let vote_stake_list = self.vote_stake_list.as_mut().unwrap();
        vote_stake_list.vote_lock(amount, unlock_block_number)
    }

    pub fn remove_expired_vote_stake_info(&mut self, block_number: u64) {
        self.address.assert_native();
        assert!(self.vote_stake_list.is_some());
        let vote_stake_list = self.vote_stake_list.as_mut().unwrap();
        vote_stake_list.remove_expired_vote_stake_info(block_number)
    }
}
