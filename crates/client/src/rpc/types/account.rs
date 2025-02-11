// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use super::RpcAddress;
use mazze_addr::Network;
use mazze_types::{H256, U256};
use primitives::Account as PrimitiveAccount;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    // This field isn't part of Account RLP but is helpful for debugging.
    pub address: RpcAddress,
    pub balance: U256,
    pub nonce: U256,
    pub code_hash: H256,
    pub collateral_for_storage: U256,
    pub admin: RpcAddress,
}

impl Account {
    pub fn try_from(
        account: PrimitiveAccount, network: Network,
    ) -> Result<Self, String> {
        let collateral_for_storage = account.collateral_for_storage
            + account
                .sponsor_info
                .storage_points
                .as_ref()
                .map_or(U256::zero(), |x| x.used);
        Ok(Self {
            address: RpcAddress::try_from_h160(
                account.address().address,
                network,
            )?,
            balance: account.balance.into(),
            nonce: account.nonce.into(),
            code_hash: account.code_hash.into(),
            collateral_for_storage,
            admin: RpcAddress::try_from_h160(account.admin, network)?,
        })
    }
}
