use crate::rpc::types::address::RpcAddress;
use mazze_addr::Network;
use mazze_types::H256;
use primitives::{AccessList, AccessListItem};
use std::convert::Into;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MazzeAccessListItem {
    pub address: RpcAddress,
    pub storage_keys: Vec<H256>,
}

impl Into<AccessListItem> for MazzeAccessListItem {
    fn into(self) -> AccessListItem {
        AccessListItem {
            address: self.address.hex_address,
            storage_keys: self.storage_keys,
        }
    }
}

pub type MazzeAccessList = Vec<MazzeAccessListItem>;

pub fn to_primitive_access_list(list: MazzeAccessList) -> AccessList {
    list.into_iter().map(|item| item.into()).collect()
}

pub fn from_primitive_access_list(
    list: AccessList, network: Network,
) -> MazzeAccessList {
    list.into_iter()
        .map(|item| MazzeAccessListItem {
            address: RpcAddress::try_from_h160(item.address, network).unwrap(),
            storage_keys: item.storage_keys,
        })
        .collect()
}
