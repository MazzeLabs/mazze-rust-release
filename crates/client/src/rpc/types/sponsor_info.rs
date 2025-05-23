// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use super::RpcAddress;
use mazze_addr::Network;
use mazze_parameters::collateral::MAZZIES_PER_STORAGE_COLLATERAL_UNIT;
use mazze_types::U256;
use primitives::SponsorInfo as PrimitiveSponsorInfo;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SponsorInfo {
    /// This is the address of the sponsor for gas cost of the contract.
    pub sponsor_for_gas: RpcAddress,
    /// This is the address of the sponsor for collateral of the contract.
    pub sponsor_for_collateral: RpcAddress,
    /// This is the upper bound of sponsor gas cost per tx.
    pub sponsor_gas_bound: U256,
    /// This is the amount of tokens sponsor for gas cost to the contract.
    pub sponsor_balance_for_gas: U256,
    /// This is the amount of tokens sponsor for collateral to the contract.
    pub sponsor_balance_for_collateral: U256,
    /// This is the amount of unused storage points (in terms of bytes).
    pub available_storage_points: U256,
    /// This is the amount of used storage points (in terms of bytes).
    pub used_storage_points: U256,
}

impl SponsorInfo {
    pub fn default(network: Network) -> Result<Self, String> {
        Ok(Self {
            sponsor_for_gas: RpcAddress::null(network)?,
            sponsor_for_collateral: RpcAddress::null(network)?,
            sponsor_gas_bound: Default::default(),
            sponsor_balance_for_gas: Default::default(),
            sponsor_balance_for_collateral: Default::default(),
            available_storage_points: Default::default(),
            used_storage_points: Default::default(),
        })
    }

    pub fn try_from(
        sponsor_info: PrimitiveSponsorInfo, network: Network,
    ) -> Result<Self, String> {
        Ok(Self {
            sponsor_for_gas: RpcAddress::try_from_h160(
                sponsor_info.sponsor_for_gas,
                network,
            )?,
            sponsor_for_collateral: RpcAddress::try_from_h160(
                sponsor_info.sponsor_for_collateral,
                network,
            )?,
            sponsor_gas_bound: sponsor_info.sponsor_gas_bound,
            sponsor_balance_for_gas: sponsor_info.sponsor_balance_for_gas,
            sponsor_balance_for_collateral: sponsor_info
                .sponsor_balance_for_collateral,
            available_storage_points: sponsor_info
                .storage_points
                .as_ref()
                .map_or(U256::zero(), |x| {
                    x.unused / *MAZZIES_PER_STORAGE_COLLATERAL_UNIT
                }),
            used_storage_points: sponsor_info
                .storage_points
                .as_ref()
                .map_or(U256::zero(), |x| {
                    x.used / *MAZZIES_PER_STORAGE_COLLATERAL_UNIT
                }),
        })
    }
}
