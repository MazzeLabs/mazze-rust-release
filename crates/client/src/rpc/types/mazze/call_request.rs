// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::rpc::{
    error_codes::invalid_params,
    types::{
        address::RpcAddress,
        errors::{check_rpc_address_network, RcpAddressNetworkInconsistent},
        mazze::{to_primitive_access_list, MazzeAccessList},
        Bytes,
    },
    RpcResult,
};
use mazze_addr::Network;
use mazze_types::{Address, AddressSpaceUtil, U256, U64};
use mazzecore::rpc_errors::invalid_params_check;
use mazzecore_accounts::AccountProvider;
use mazzekey::Password;
use primitives::{
    transaction::{
        native_transaction::NativeTransaction as PrimitiveTransaction, Action,
        Mip1559Transaction, Mip2930Transaction, NativeTransaction,
        ShieldedTransaction, TypedNativeTransaction::*, LEGACY_TX_TYPE,
        MIP1559_TYPE, MIP2930_TYPE, MIP_SHIELDED_TYPE,
    },
    SignedTransaction, Transaction, TransactionWithSignature,
};
use std::{cmp::min, convert::Into, sync::Arc};

/// The MAX_GAS_CALL_REQUEST is used as max value of mazze_call or
/// mazze_estimate's gas value to prevent call_virtual consumes too much
/// resource. The tx_pool will reject the tx if the gas is larger than half of
/// the block gas limit. which is 30_000_000 before 1559, and 60_000_000 after
/// 1559.
pub const MAX_GAS_CALL_REQUEST: u64 = 15_000_000;

#[derive(Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallRequest {
    /// From
    pub from: Option<RpcAddress>,
    /// To
    pub to: Option<RpcAddress>,
    /// Gas Price
    pub gas_price: Option<U256>,
    /// Gas
    pub gas: Option<U256>,
    /// Value
    pub value: Option<U256>,
    /// Data
    pub data: Option<Bytes>,
    /// Nonce
    pub nonce: Option<U256>,
    /// StorageLimit
    pub storage_limit: Option<U64>,
    /// Access list in EIP-2930
    pub access_list: Option<MazzeAccessList>,
    pub max_fee_per_gas: Option<U256>,
    pub max_priority_fee_per_gas: Option<U256>,
    #[serde(rename = "type")]
    pub transaction_type: Option<U64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendTxRequest {
    pub from: RpcAddress,
    pub to: Option<RpcAddress>,
    pub gas: U256,
    pub gas_price: U256,
    pub value: U256,
    pub data: Option<Bytes>,
    pub nonce: Option<U256>,
    pub storage_limit: Option<U256>,
    pub chain_id: Option<U256>,
    pub epoch_height: Option<U256>,
    #[serde(rename = "type")]
    pub transaction_type: Option<U64>,
}

#[derive(Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EstimateGasAndCollateralResponse {
    /// The recommended gas_limit.
    pub gas_limit: U256,
    /// The amount of gas used in the execution.
    pub gas_used: U256,
    /// The number of bytes collateralized in the execution.
    pub storage_collateralized: U64,
}

#[derive(Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckBalanceAgainstTransactionResponse {
    /// Whether the account should pay transaction fee by self.
    pub will_pay_tx_fee: bool,
    /// Whether the account should pay collateral by self.
    pub will_pay_collateral: bool,
    /// Whether the account balance is enough for this transaction.
    pub is_balance_enough: bool,
}

impl SendTxRequest {
    pub fn check_rpc_address_network(
        &self, param_name: &str, expected: &Network,
    ) -> RpcResult<()> {
        let rpc_request_network = invalid_params_check(
            param_name,
            rpc_call_request_network(Some(&self.from), self.to.as_ref()),
        )?;
        invalid_params_check(
            param_name,
            check_rpc_address_network(rpc_request_network, expected),
        )
    }

    pub fn sign_with(
        self, best_epoch_height: u64, chain_id: u32, password: Option<String>,
        accounts: Arc<AccountProvider>,
    ) -> RpcResult<TransactionWithSignature> {
        let nonce = self.nonce.unwrap_or_default();
        let gas_price = self.gas_price;
        let gas = self.gas;
        let action = match self.to {
            None => Action::Create,
            Some(address) => Action::Call(address.into()),
        };
        let value = self.value;
        let storage_limit =
            self.storage_limit.unwrap_or_default().as_usize() as u64;
        let epoch_height = self
            .epoch_height
            .unwrap_or(best_epoch_height.into())
            .as_usize() as u64;
        let chain_id = self.chain_id.unwrap_or(chain_id.into()).as_u32();
        let data: mazze_bytes::Bytes =
            self.data.unwrap_or(Bytes::new(vec![])).into();
        let tx_type = self.transaction_type.map(|id| id.as_usize() as u8);

        if matches!(tx_type, Some(MIP_SHIELDED_TYPE)) {
            return Err(
                "Shielded transactions must be submitted as unsigned raw transactions"
                    .into(),
            );
        }

        if epoch_height == u64::MAX {
            return Err("Can not sign Ethereum like transaction by RPC.".into());
        }

        let password = password.map(Password::from);
        let (tx, sig_hash) = match tx_type {
            Some(MIP_SHIELDED_TYPE) => {
                if matches!(action, Action::Create) {
                    return Err(
                        "Shielded transaction requires a recipient".into()
                    );
                }
                if data.is_empty() {
                    return Err("Shielded payload must not be empty".into());
                }
                if !value.is_zero() {
                    return Err(
                        "Shielded transaction must not transfer value".into()
                    );
                }
                let tx = ShieldedTransaction {
                    nonce: nonce.into(),
                    gas_price: gas_price.into(),
                    gas: gas.into(),
                    action,
                    value: value.into(),
                    storage_limit,
                    epoch_height,
                    chain_id,
                    data,
                };
                let sig_hash =
                    Transaction::Native(Shielded(tx.clone())).signature_hash();
                (Transaction::Native(Shielded(tx)), sig_hash)
            }
            Some(other) => {
                return Err(format!(
                    "Unsupported transaction type {} for signTransaction",
                    other
                )
                .into());
            }
            None => {
                let tx = PrimitiveTransaction {
                    nonce: nonce.into(),
                    gas_price: gas_price.into(),
                    gas: gas.into(),
                    action,
                    value: value.into(),
                    storage_limit,
                    epoch_height,
                    chain_id,
                    data,
                };
                let sig_hash = Transaction::from(tx.clone()).signature_hash();
                (Transaction::from(tx), sig_hash)
            }
        };

        let sig = accounts
            .sign(self.from.into(), password, sig_hash)
            // TODO: sign error into secret store error codes.
            .map_err(|e| format!("failed to sign transaction: {:?}", e))?;

        Ok(tx.with_signature(sig))
    }
}

pub fn sign_call(
    epoch_height: u64, chain_id: u32, request: CallRequest,
) -> RpcResult<SignedTransaction> {
    let max_gas = U256::from(MAX_GAS_CALL_REQUEST);
    let gas = min(request.gas.unwrap_or(max_gas), max_gas);

    let nonce = request.nonce.unwrap_or_default();
    let action = request.to.map_or(Action::Create, |rpc_addr| {
        Action::Call(rpc_addr.hex_address)
    });

    let value = request.value.unwrap_or_default();
    let storage_limit = request
        .storage_limit
        .map(|v| v.as_u64())
        .unwrap_or(std::u64::MAX);
    let data = request.data.unwrap_or_default().into_vec();

    let default_type_id = if request.max_fee_per_gas.is_some()
        || request.max_priority_fee_per_gas.is_some()
    {
        MIP1559_TYPE
    } else if request.access_list.is_some() {
        MIP2930_TYPE
    } else {
        LEGACY_TX_TYPE
    };
    let transaction_type = request
        .transaction_type
        .unwrap_or(U64::from(default_type_id));

    if transaction_type.as_usize() as u8 == MIP_SHIELDED_TYPE {
        if matches!(action, Action::Create) {
            return Err(invalid_params(
                "transaction_type",
                "shielded transaction requires a recipient",
            )
            .into());
        }
        if data.is_empty() {
            return Err(invalid_params(
                "data",
                "shielded payload must not be empty",
            )
            .into());
        }
        if !value.is_zero() {
            return Err(invalid_params(
                "value",
                "shielded transaction must not transfer value",
            )
            .into());
        }
    }

    let gas_price = request.gas_price.unwrap_or(1.into());
    let max_fee_per_gas = request
        .max_fee_per_gas
        .or(request.max_priority_fee_per_gas)
        .unwrap_or(gas_price);
    let max_priority_fee_per_gas =
        request.max_priority_fee_per_gas.unwrap_or(U256::zero());
    let access_list = request.access_list.unwrap_or(vec![]);

    let transaction = match transaction_type.as_usize() as u8 {
        LEGACY_TX_TYPE => Mip155(NativeTransaction {
            nonce,
            action,
            gas,
            gas_price,
            value,
            storage_limit,
            epoch_height,
            chain_id,
            data,
        }),
        MIP2930_TYPE => Mip2930(Mip2930Transaction {
            nonce,
            gas_price,
            gas,
            action,
            value,
            storage_limit,
            epoch_height,
            chain_id,
            data,
            access_list: to_primitive_access_list(access_list),
        }),
        MIP1559_TYPE => Mip1559(Mip1559Transaction {
            nonce,
            action,
            gas,
            value,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            storage_limit,
            epoch_height,
            chain_id,
            data,
            access_list: to_primitive_access_list(access_list),
        }),
        MIP_SHIELDED_TYPE => Shielded(ShieldedTransaction {
            nonce,
            action,
            gas,
            gas_price,
            value,
            storage_limit,
            epoch_height,
            chain_id,
            data,
        }),
        x => {
            return Err(
                invalid_params("Unrecognized transaction type", x).into()
            );
        }
    };

    if matches!(transaction, Shielded(_)) {
        let unsigned = Transaction::Native(transaction);
        let tx_with_sig = TransactionWithSignature::new_unsigned(unsigned);
        return Ok(SignedTransaction::new_shielded(tx_with_sig));
    }

    let from = request
        .from
        .map_or_else(|| Address::zero(), |rpc_addr| rpc_addr.hex_address);

    Ok(transaction.fake_sign_rpc(from.with_native_space()))
}

pub fn rpc_call_request_network(
    from: Option<&RpcAddress>, to: Option<&RpcAddress>,
) -> Result<Option<Network>, RcpAddressNetworkInconsistent> {
    let request_network = from.map(|rpc_addr| rpc_addr.network);
    match request_network {
        None => Ok(to.map(|rpc_addr| rpc_addr.network)),
        Some(network) => {
            if let Some(to) = to {
                if to.network != network {
                    return Err(RcpAddressNetworkInconsistent {
                        from_network: network,
                        to_network: to.network,
                    });
                }
            }
            Ok(Some(network))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CallRequest;

    use crate::rpc::types::address::RpcAddress;
    use mazze_addr::Network;
    use mazze_types::{H160, U256, U64};
    use rustc_hex::FromHex;
    use serde_json;
    use std::str::FromStr;

    #[test]
    fn call_request_deserialize() {
        let expected = CallRequest {
            from: Some(
                RpcAddress::try_from_h160(
                    H160::from_low_u64_be(1),
                    Network::Main,
                )
                .unwrap(),
            ),
            to: Some(
                RpcAddress::try_from_h160(
                    H160::from_low_u64_be(2),
                    Network::Main,
                )
                .unwrap(),
            ),
            gas_price: Some(U256::from(1)),
            gas: Some(U256::from(2)),
            value: Some(U256::from(3)),
            data: Some(vec![0x12, 0x34, 0x56].into()),
            storage_limit: Some(U64::from_str("7b").unwrap()),
            nonce: Some(U256::from(4)),
            ..Default::default()
        };

        let s = r#"{
            "from":"MAZZE:TYPE.BUILTIN:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAEJC4EYEY6",
            "to":"MAZZE:TYPE.BUILTIN:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAJD0WN6U9U",
            "gasPrice":"0x1",
            "gas":"0x2",
            "value":"0x3",
            "data":"0x123456",
            "storageLimit":"0x7b",
            "nonce":"0x4"
        }"#;
        let deserialized_result = serde_json::from_str::<CallRequest>(s);
        assert!(
            deserialized_result.is_ok(),
            "serialized str should look like {}",
            serde_json::to_string(&expected).unwrap()
        );
        assert_eq!(deserialized_result.unwrap(), expected);
    }

    #[test]
    fn call_request_deserialize2() {
        let expected = CallRequest {
            from: Some(RpcAddress::try_from_h160(H160::from_str("160e8dd61c5d32be8058bb8eb970870f07233155").unwrap(),  Network::Main ).unwrap()),
            to: Some(RpcAddress::try_from_h160(H160::from_str("846e8dd67c5d32be8058bb8eb970870f07244567").unwrap(), Network::Main).unwrap()),
            gas_price: Some(U256::from_str("9184e72a000").unwrap()),
            gas: Some(U256::from_str("76c0").unwrap()),
            value: Some(U256::from_str("9184e72a").unwrap()),
            storage_limit: Some(U64::from_str("3344adf").unwrap()),
            data: Some("d46e8dd67c5d32be8d46e8dd67c5d32be8058bb8eb970870f072445675058bb8eb970870f072445675".from_hex::<Vec<u8>>().unwrap().into()),
            nonce: None,
            ..Default::default()
        };

        let s = r#"{
            "from": "MAZZE:TYPE.USER:AANA7DS0DVSXFTYANC727SNUU6HUSJ3VMYC3F1AY93",
            "to": "MAZZE:TYPE.CONTRACT:ACCG7DS0TVSXFTYANC727SNUU6HUSKCFP6KB3NFJ02",
            "gas": "0x76c0",
            "gasPrice": "0x9184e72a000",
            "value": "0x9184e72a",
            "storageLimit":"0x3344adf",
            "data": "0xd46e8dd67c5d32be8d46e8dd67c5d32be8058bb8eb970870f072445675058bb8eb970870f072445675"
        }"#;
        let deserialized_result = serde_json::from_str::<CallRequest>(s);
        assert!(
            deserialized_result.is_ok(),
            "serialized str should look like {}",
            serde_json::to_string(&expected).unwrap()
        );
        assert_eq!(deserialized_result.unwrap(), expected);
    }

    #[test]
    fn call_request_deserialize_empty() {
        let expected = CallRequest {
            from: Some(
                RpcAddress::try_from_h160(
                    H160::from_low_u64_be(1),
                    Network::Main,
                )
                .unwrap(),
            ),
            to: None,
            gas_price: None,
            gas: None,
            value: None,
            data: None,
            storage_limit: None,
            nonce: None,
            ..Default::default()
        };

        let s = r#"{"from":"MAZZE:TYPE.BUILTIN:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAEJC4EYEY6"}"#;
        let deserialized_result = serde_json::from_str::<CallRequest>(s);
        assert!(
            deserialized_result.is_ok(),
            "serialized str should look like {}",
            serde_json::to_string(&expected).unwrap()
        );
        assert_eq!(deserialized_result.unwrap(), expected);
    }
}
