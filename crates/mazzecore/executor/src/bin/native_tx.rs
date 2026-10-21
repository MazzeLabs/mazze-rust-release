// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use mazze_parameters::consensus::ONE_MAZZE_IN_MAZZY;
use mazze_types::{Address, U256};
use mazzekey::{KeyPair, Secret};
use primitives::{
    transaction::{
        native_transaction::NativeTransaction, TypedNativeTransaction,
    },
    Action, Transaction, TransactionWithSignature,
};
use rustc_hex::{FromHex, ToHex};
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
                value =
                    Some(parse_u256(args.get(i).ok_or("missing --value")?)?);
            }
            "--value-mazze" => {
                i += 1;
                value_mazze = Some(parse_u256(
                    args.get(i).ok_or("missing --value-mazze")?,
                )?);
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
            _ => {
                return Err(format!("unknown arg: {}", args[i]));
            }
        }
        i += 1;
    }

    let from_secret = from_secret
        .ok_or("missing --from-secret")?
        .trim()
        .to_string();
    let secret: Secret = from_secret
        .parse()
        .map_err(|e| format!("invalid secret: {:?}", e))?;
    let _ =
        KeyPair::from_secret(secret.clone()).map_err(|e| format!("{:?}", e))?;

    let value = match (value, value_mazze) {
        (Some(raw), None) => raw,
        (None, Some(mazze)) => mazze * U256::from(ONE_MAZZE_IN_MAZZY),
        (Some(_), Some(_)) => {
            return Err("use either --value or --value-mazze".into())
        }
        (None, None) => U256::zero(),
    };

    let action = match to {
        Some(addr) => Action::Call(parse_address(&addr)?),
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

    let signed =
        Transaction::Native(TypedNativeTransaction::Mip155(tx)).sign(&secret);
    let signed_tx: TransactionWithSignature = signed.into();
    let raw = rlp::encode(&signed_tx);

    println!("0x{}", raw.to_hex::<String>());
    Ok(())
}
