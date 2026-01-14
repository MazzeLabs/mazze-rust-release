// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use mazzekey::{KeyPair, Secret};
use mazze_parameters::consensus::ONE_MAZZE_IN_MAZZY;
use mazze_parameters::internal_contract_addresses::SHIELDED_POOL_CONTRACT_ADDRESS;
use mazze_types::{Address, H256, U256};
use primitives::{
    transaction::{native_transaction::NativeTransaction, TypedNativeTransaction},
    Action, Transaction, TransactionWithSignature,
};
use rustc_hex::{FromHex, ToHex};
use sha3_macro::keccak;
use solidity_abi::ABIListWriter;
use std::str::FromStr;

fn parse_u256(input: &str) -> Result<U256, String> {
    let trimmed = input.trim();
    if let Some(hex) = trimmed.strip_prefix("0x") {
        let hex = if hex.len() % 2 == 1 {
            format!("0{}", hex)
        } else {
            hex.to_string()
        };
        let bytes: Vec<u8> = hex
            .from_hex()
            .map_err(|e| format!("invalid hex number {}: {:?}", input, e))?;
        if bytes.len() > 32 {
            return Err(format!("hex number too large: {}", input));
        }
        let mut buf = [0u8; 32];
        buf[32 - bytes.len()..].copy_from_slice(&bytes);
        Ok(U256::from_big_endian(&buf))
    } else {
        U256::from_dec_str(trimmed)
            .map_err(|e| format!("invalid decimal number {}: {:?}", input, e))
    }
}

fn parse_u64(input: &str) -> Result<u64, String> {
    let trimmed = input.trim();
    if let Some(hex) = trimmed.strip_prefix("0x") {
        u64::from_str_radix(hex, 16)
            .map_err(|e| format!("invalid hex number {}: {:?}", input, e))
    } else {
        u64::from_str(trimmed)
            .map_err(|e| format!("invalid decimal number {}: {:?}", input, e))
    }
}

fn parse_h256(input: &str) -> Result<H256, String> {
    let trimmed = input.trim();
    let hex = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    hex.parse::<H256>()
        .map_err(|e| format!("invalid H256 {}: {:?}", input, e))
}

fn parse_bytes(input: &str) -> Result<Vec<u8>, String> {
    let trimmed = input.trim();
    let hex = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    if hex.len() % 2 != 0 {
        return Err(format!("hex string must be even length: {}", input));
    }
    hex.from_hex()
        .map_err(|e| format!("invalid hex bytes {}: {:?}", input, e))
}

fn parse_address(input: &str) -> Result<Address, String> {
    let trimmed = input.trim();
    let hex = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    Address::from_str(hex)
        .map_err(|e| format!("invalid address {}: {:?}", input, e))
}

fn main() -> Result<(), String> {
    let mut from_secret = None;
    let mut to = None;
    let mut value = None;
    let mut value_mazze = None;
    let mut gas = U256::from(21_000u64);
    let mut gas_price = U256::from(1u64);
    let mut storage_limit = 0u64;
    let mut nonce = U256::zero();
    let mut chain_id = 0u32;
    let mut epoch_height = 0u64;
    let mut data = Vec::new();
    let mut shield_commitment = None;
    let mut shield_ciphertext = None;

    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from-secret" => {
                i += 1;
                from_secret = args.get(i).cloned();
            }
            "--to" => {
                i += 1;
                to = args.get(i).cloned();
            }
            "--value" => {
                i += 1;
                value = Some(parse_u256(args.get(i).ok_or("missing --value")?)?);
            }
            "--value-mazze" => {
                i += 1;
                value_mazze =
                    Some(parse_u256(args.get(i).ok_or("missing --value-mazze")?)?);
            }
            "--gas" => {
                i += 1;
                gas = parse_u256(args.get(i).ok_or("missing --gas")?)?;
            }
            "--gas-price" => {
                i += 1;
                gas_price =
                    parse_u256(args.get(i).ok_or("missing --gas-price")?)?;
            }
            "--storage-limit" => {
                i += 1;
                storage_limit =
                    parse_u64(args.get(i).ok_or("missing --storage-limit")?)?;
            }
            "--nonce" => {
                i += 1;
                nonce = parse_u256(args.get(i).ok_or("missing --nonce")?)?;
            }
            "--chain-id" => {
                i += 1;
                chain_id =
                    parse_u64(args.get(i).ok_or("missing --chain-id")?)? as u32;
            }
            "--epoch" | "--epoch-height" => {
                i += 1;
                epoch_height =
                    parse_u64(args.get(i).ok_or("missing --epoch-height")?)?;
            }
            "--data" => {
                i += 1;
                let hex = args.get(i).ok_or("missing --data")?;
                let hex = hex.strip_prefix("0x").unwrap_or(hex);
                data = hex
                    .from_hex()
                    .map_err(|e| format!("invalid hex data: {:?}", e))?;
            }
            "--shield-commitment" => {
                i += 1;
                shield_commitment =
                    Some(parse_h256(args.get(i).ok_or("missing --shield-commitment")?)?);
            }
            "--shield-ciphertext" => {
                i += 1;
                shield_ciphertext = Some(parse_bytes(
                    args.get(i).ok_or("missing --shield-ciphertext")?,
                )?);
            }
            _ => {
                return Err(format!("unknown arg: {}", args[i]));
            }
        }
        i += 1;
    }

    let from_secret =
        from_secret.ok_or("missing --from-secret")?.trim().to_string();
    let secret: Secret = from_secret
        .parse()
        .map_err(|e| format!("invalid secret: {:?}", e))?;
    let _ = KeyPair::from_secret(secret.clone())
        .map_err(|e| format!("{:?}", e))?;

    let value = match (value, value_mazze) {
        (Some(raw), None) => raw,
        (None, Some(mazze)) => mazze * U256::from(ONE_MAZZE_IN_MAZZY),
        (Some(_), Some(_)) => return Err("use either --value or --value-mazze".into()),
        (None, None) => U256::zero(),
    };

    let to_address = match (to, shield_commitment) {
        (Some(addr), None) => Some(parse_address(&addr)?),
        (None, Some(_)) => Some(SHIELDED_POOL_CONTRACT_ADDRESS),
        (Some(addr), Some(_)) => {
            let parsed = parse_address(&addr)?;
            if parsed != SHIELDED_POOL_CONTRACT_ADDRESS {
                return Err(
                    "shielded commitment requires shielded pool address".into(),
                );
            }
            Some(parsed)
        }
        (None, None) => None,
    };

    if let Some(commitment) = shield_commitment {
        let ciphertext = shield_ciphertext
            .filter(|c| !c.is_empty())
            .ok_or("missing --shield-ciphertext")?;
        let selector = keccak!("shield(bytes32,bytes)");
        let mut writer = ABIListWriter::with_heads_length(2 * 32);
        writer.write_down(&commitment);
        writer.write_down(&ciphertext);
        let encoded = writer.into_linked_bytes();
        data.clear();
        data.extend_from_slice(&selector[0..4]);
        data.extend_from_slice(&encoded.to_vec());
    }

    let action = match to_address {
        Some(addr) => Action::Call(addr),
        None => Action::Create,
    };

    let tx = NativeTransaction {
        nonce,
        gas_price,
        gas,
        action,
        value,
        storage_limit,
        epoch_height,
        chain_id,
        data: data.into(),
    };

    let signed = Transaction::Native(TypedNativeTransaction::Mip155(tx))
        .sign(&secret);
    let signed_tx: TransactionWithSignature = signed.into();
    let raw = rlp::encode(&signed_tx);

    println!("0x{}", raw.to_hex::<String>());
    Ok(())
}
