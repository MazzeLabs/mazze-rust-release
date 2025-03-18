// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use mazze_bytes::Bytes;
use mazze_internal_common::{ChainIdParams, ChainIdParamsInner};
use mazze_parameters::{
    block::{EVM_TRANSACTION_BLOCK_RATIO, EVM_TRANSACTION_GAS_RATIO},
    consensus::ONE_UMAZZE_IN_MAZZY,
    consensus_internal::{
        HALVING_INTERVAL_IN_BLOCKS, INITIAL_BASE_MINING_REWARD_IN_UMAZZE,
        OUTLIER_PENALTY_RATIO,
    },
};
use mazze_types::{AllChainID, Space, SpaceMap, U256, U512};
use mazze_vm_types::Spec;
use primitives::{block::BlockHeight, BlockNumber};
use std::collections::BTreeMap;

// FIXME: This type is mainly used for execution layer parameters, but some
// consensus layer parameters and functions are also inappropriately placed
// here.

#[derive(Debug)]
pub struct CommonParams {
    /// Maximum size of extra data.
    pub maximum_extra_data_size: usize,
    /// Network id.
    pub network_id: u64,
    /// Chain id.
    pub chain_id: ChainIdParams,
    /// Main subprotocol name.
    pub subprotocol_name: String,
    /// Minimum gas limit.
    pub min_gas_limit: U256,
    /// Gas limit bound divisor (how much gas limit can change per block)
    pub gas_limit_bound_divisor: U256,
    /// Number of first block where max code size limit is active.
    /// Maximum size of transaction's RLP payload.
    pub max_transaction_size: usize,
    /// Outlier penalty ratio for reward processing.
    /// It should be less than `timer_chain_beta`.
    pub outlier_penalty_ratio: u64,
    /// Initial base rewards according to block height.
    pub base_block_rewards: BTreeMap<BlockHeight, U256>,
    /// The ratio of blocks in the EVM transactions
    pub evm_transaction_block_ratio: u64,
    /// The gas ratio of evm transactions for the block can pack the EVM
    /// transactions
    pub evm_transaction_gas_ratio: u64,
    pub min_base_price: SpaceMap<U256>,

    /// Set the internal contracts to state at the genesis blocks, even if it
    /// is not activated.
    pub early_set_internal_contracts_states: bool,
    /// The upgrades activated at given block number.
    pub transition_numbers: TransitionsBlockNumber,
    /// The upgrades activated at given block height (a.k.a. epoch number).
    pub transition_heights: TransitionsEpochHeight,
}

#[derive(Default, Debug, Clone)]
pub struct TransitionsBlockNumber {
    /// CIP-141: Disable Subroutine Opcodes
    /// CIP-142: Transient Storage Opcodes
    /// CIP-143: MCOPY (0x5e) Opcode for Efficient Memory Copy
    pub cancun_opcodes: BlockNumber,
}

/// Empty struct for now, will be used for epoch height transitions.
#[derive(Default, Debug, Clone)]
pub struct TransitionsEpochHeight {}

impl Default for CommonParams {
    fn default() -> Self {
        CommonParams {
            maximum_extra_data_size: 0x20,
            network_id: 0x1,
            chain_id: ChainIdParamsInner::new_simple(AllChainID::new(1, 1)),
            subprotocol_name: "mazze".into(),
            min_gas_limit: 10_000_000.into(),
            gas_limit_bound_divisor: 0x0400.into(),
            max_transaction_size: 300 * 1024,
            outlier_penalty_ratio: OUTLIER_PENALTY_RATIO,
            base_block_rewards: Self::get_block_rewards_config(),
            evm_transaction_block_ratio: EVM_TRANSACTION_BLOCK_RATIO,
            evm_transaction_gas_ratio: EVM_TRANSACTION_GAS_RATIO,
            early_set_internal_contracts_states: false,
            transition_numbers: Default::default(),
            transition_heights: Default::default(),
            min_base_price: SpaceMap::default(),
        }
    }
}

impl CommonParams {
    pub fn spec(&self, number: BlockNumber, _height: BlockHeight) -> Spec {
        let mut spec = Spec::genesis_spec();

        spec.cancun_opcodes = number >= self.transition_numbers.cancun_opcodes;
        if spec.cancun_opcodes {
            spec.sload_gas = 800;
        }
        spec
    }

    #[cfg(test)]
    pub fn spec_for_test(&self, number: u64) -> Spec {
        self.spec(number, number)
    }

    /// Return the base reward for a block.
    /// `past_block_count` may be used for reward decay again in the future.
    pub fn base_reward_in_umazze(
        &self, _past_block_count: u64, height: BlockHeight,
    ) -> U512 {
        let (_, start_base_ward) = self.base_block_rewards.iter()
            .rev()
            .find(|&(block, _)| *block <= height)
            .expect("Current block's reward is not found; this indicates a chain config error");
        // Possible decay computation based on past_block_count.
        U512::from(start_base_ward) * U512::from(ONE_UMAZZE_IN_MAZZY)
    }

    pub fn custom_prefix(&self, _height: BlockHeight) -> Option<Vec<Bytes>> {
        None
    }

    pub fn can_pack_evm_transaction(&self, height: BlockHeight) -> bool {
        height % self.evm_transaction_block_ratio == 0
    }

    pub fn chain_id(&self, epoch_height: u64, space: Space) -> u32 {
        self.chain_id
            .read()
            .get_chain_id(epoch_height)
            .in_space(space)
    }

    pub fn chain_id_map(&self, epoch_height: u64) -> BTreeMap<Space, u32> {
        BTreeMap::from([
            (Space::Native, self.chain_id(epoch_height, Space::Native)),
            (
                Space::Ethereum,
                self.chain_id(epoch_height, Space::Ethereum),
            ),
        ])
    }

    pub fn init_base_price(&self) -> SpaceMap<U256> {
        self.min_base_price
    }

    pub fn min_base_price(&self) -> SpaceMap<U256> {
        self.min_base_price
    }

    fn get_block_rewards_config() -> BTreeMap<u64, U256> {
        let mut base_block_rewards = BTreeMap::new();

        // Initial reward
        let mut current_reward = INITIAL_BASE_MINING_REWARD_IN_UMAZZE;
        let mut blocks_passed = 0;

        // Keep adding halving periods until reward reaches zero through integer division
        while current_reward > 0 {
            // Add entry for this reward period
            base_block_rewards
                .insert(blocks_passed, U256::from(current_reward));

            // Move to next halving interval
            blocks_passed += HALVING_INTERVAL_IN_BLOCKS;

            // Halve the reward (integer division)
            current_reward /= 2;
        }

        // Add final zero entry to mark end of issuance
        if !base_block_rewards.contains_key(&blocks_passed) {
            base_block_rewards.insert(blocks_passed, U256::from(0));
        }

        base_block_rewards
    }
}

#[cfg(test)]
mod tests {

    use mazze_parameters::{
        consensus::ONE_MAZZE_IN_UMAZZE,
        consensus_internal::{
            GENESIS_TOKEN_COUNT_IN_MAZZE, MAX_SUPPLY_TOKEN_COUNT_IN_MAZZE,
        },
    };
    use mazze_types::U256;

    #[test]
    fn test_total_issuance_matches_max_supply() {
        let schedule = CommonParams::get_block_rewards_config();

        let mut issuance = String::new();

        let total =
            schedule.iter().fold(U256::zero(), |acc, (height, reward)| {
                let next_change = schedule
                    .range((height + 1)..)
                    .next()
                    .map(|(h, _)| *h)
                    .unwrap_or(u64::MAX);
                let blocks_at_this_reward = next_change - height;
                issuance.push_str(&format!(
                    "{}\t{}\t{}\n",
                    height, reward, blocks_at_this_reward
                ));
                acc + (*reward * U256::from(blocks_at_this_reward))
            });

        let expected = U256::from(
            MAX_SUPPLY_TOKEN_COUNT_IN_MAZZE - GENESIS_TOKEN_COUNT_IN_MAZZE,
        ) * U256::from(ONE_MAZZE_IN_UMAZZE);

        // With Bitcoin-style halving, we expect slightly less than the maximum supply
        // due to integer division rounding

        // Ensure we don't exceed max supply
        assert!(total <= expected, "Total issuance exceeds max supply");

        // Check that we're reasonably close (within 0.001% of max supply)
        let difference = expected - total;
        let max_acceptable_difference = expected / 100_000; // 0.001% difference

        assert!(
            difference <= max_acceptable_difference,
            "Total issuance is too far below max supply. Difference: {} ({}%)",
            difference,
            (difference * U256::from(100_000)) / expected
        );

        // Print the actual percentage of max supply distributed
        println!(
            "Issued {}/{} tokens ({}%)",
            total,
            expected,
            (total * U256::from(100)) / expected
        );
    }
}
