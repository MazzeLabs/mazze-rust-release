// Copyright 2025 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Minimal serde types for the Ethereum `GeneralStateTests` JSON format.
//!
//! We deserialize only the fields we actually use; unknown fields are
//! ignored. The full upstream schema is much larger.
//!
//! Reference: <https://github.com/ethereum/tests/blob/develop/docs/general_state_tests_format.md>

use mazze_types::{Address, H256, U256};
use serde::{Deserialize, Deserializer};
use std::collections::HashMap;

/// Top-level: each fixture file is a JSON object mapping a test name to
/// a single `Case`.
pub type TestFile = HashMap<String, Case>;

#[derive(Deserialize, Debug)]
pub struct Case {
    pub env: TestEnv,
    pub pre: HashMap<HexAddress, PreAccount>,
    pub transaction: Transaction,
    pub post: HashMap<String, Vec<PostEntry>>,
}

#[derive(Deserialize, Debug)]
pub struct TestEnv {
    #[serde(rename = "currentCoinbase", deserialize_with = "hex_address")]
    pub coinbase: Address,
    #[serde(rename = "currentDifficulty", deserialize_with = "hex_u256")]
    pub difficulty: U256,
    #[serde(rename = "currentGasLimit", deserialize_with = "hex_u256")]
    pub gas_limit: U256,
    #[serde(rename = "currentNumber", deserialize_with = "hex_u256")]
    pub number: U256,
    #[serde(rename = "currentTimestamp", deserialize_with = "hex_u256")]
    pub timestamp: U256,
    #[serde(
        rename = "currentBaseFee",
        deserialize_with = "hex_u256_opt",
        default
    )]
    pub base_fee: Option<U256>,
    #[serde(
        rename = "previousHash",
        deserialize_with = "hex_h256_opt",
        default
    )]
    pub previous_hash: Option<H256>,
}

#[derive(Deserialize, Debug)]
pub struct PreAccount {
    #[serde(deserialize_with = "hex_u256")]
    pub balance: U256,
    #[serde(deserialize_with = "hex_bytes")]
    pub code: Vec<u8>,
    #[serde(deserialize_with = "hex_u256")]
    pub nonce: U256,
    #[serde(deserialize_with = "hex_storage_map")]
    pub storage: HashMap<H256, U256>,
}

#[derive(Deserialize, Debug)]
pub struct Transaction {
    #[serde(deserialize_with = "hex_address")]
    pub sender: Address,
    // `to` is empty for CREATE transactions, an address otherwise.
    #[serde(deserialize_with = "hex_address_opt", default)]
    pub to: Option<Address>,
    // These three are arrays — the index in `PostEntry::indexes` picks one.
    #[serde(deserialize_with = "vec_hex_u256")]
    pub data: Vec<Vec<u8>>,
    #[serde(rename = "gasLimit", deserialize_with = "vec_hex_u256_scalar")]
    pub gas_limit: Vec<U256>,
    #[serde(deserialize_with = "vec_hex_u256_scalar")]
    pub value: Vec<U256>,
    #[serde(rename = "gasPrice", deserialize_with = "hex_u256_opt", default)]
    pub gas_price: Option<U256>,
    #[serde(deserialize_with = "hex_u256")]
    pub nonce: U256,
}

#[derive(Deserialize, Debug)]
pub struct PostEntry {
    pub indexes: PostIndexes,
    /// State-root hash of the expected post-state.
    pub hash: H256,
    /// Logs hash of the expected log set.
    pub logs: H256,
    /// If the test expects an exception, this is `Some("...")`.
    #[serde(rename = "expectException", default)]
    pub expect_exception: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct PostIndexes {
    pub data: usize,
    pub gas: usize,
    pub value: usize,
}

// ----- Hex deserialization helpers -----

#[derive(Debug, Eq, Hash, PartialEq)]
pub struct HexAddress(pub Address);

impl<'de> Deserialize<'de> for HexAddress {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        let stripped = s.trim_start_matches("0x");
        let bytes = hex::decode(stripped).map_err(serde::de::Error::custom)?;
        if bytes.len() != 20 {
            return Err(serde::de::Error::custom(format!(
                "address must be 20 bytes, got {}",
                bytes.len()
            )));
        }
        Ok(HexAddress(Address::from_slice(&bytes)))
    }
}

fn hex_address<'de, D>(d: D) -> Result<Address, D::Error>
where
    D: Deserializer<'de>,
{
    HexAddress::deserialize(d).map(|h| h.0)
}

fn hex_address_opt<'de, D>(d: D) -> Result<Option<Address>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    let stripped = s.trim_start_matches("0x");
    if stripped.is_empty() {
        return Ok(None);
    }
    let bytes = hex::decode(stripped).map_err(serde::de::Error::custom)?;
    if bytes.len() != 20 {
        return Ok(None);
    }
    Ok(Some(Address::from_slice(&bytes)))
}

fn hex_u256<'de, D>(d: D) -> Result<U256, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    parse_u256(&s).map_err(serde::de::Error::custom)
}

fn hex_u256_opt<'de, D>(d: D) -> Result<Option<U256>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(d)?;
    opt.map(|s| parse_u256(&s).map_err(serde::de::Error::custom))
        .transpose()
}

fn hex_h256_opt<'de, D>(d: D) -> Result<Option<H256>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(d)?;
    Ok(opt.and_then(|s| {
        let stripped = s.trim_start_matches("0x");
        hex::decode(stripped)
            .ok()
            .filter(|b| b.len() == 32)
            .map(|b| H256::from_slice(&b))
    }))
}

fn hex_bytes<'de, D>(d: D) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    let stripped = s.trim_start_matches("0x");
    hex::decode(stripped).map_err(serde::de::Error::custom)
}

fn vec_hex_u256<'de, D>(d: D) -> Result<Vec<Vec<u8>>, D::Error>
where
    D: Deserializer<'de>,
{
    let v: Vec<String> = Vec::deserialize(d)?;
    v.iter()
        .map(|s| {
            let stripped = s.trim_start_matches("0x");
            hex::decode(stripped).map_err(serde::de::Error::custom)
        })
        .collect()
}

fn vec_hex_u256_scalar<'de, D>(d: D) -> Result<Vec<U256>, D::Error>
where
    D: Deserializer<'de>,
{
    let v: Vec<String> = Vec::deserialize(d)?;
    v.iter()
        .map(|s| parse_u256(s).map_err(serde::de::Error::custom))
        .collect()
}

fn hex_storage_map<'de, D>(d: D) -> Result<HashMap<H256, U256>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw: HashMap<String, String> = HashMap::deserialize(d)?;
    let mut out = HashMap::with_capacity(raw.len());
    for (k, v) in raw {
        let key = parse_h256(&k).map_err(serde::de::Error::custom)?;
        let value = parse_u256(&v).map_err(serde::de::Error::custom)?;
        out.insert(key, value);
    }
    Ok(out)
}

fn parse_u256(s: &str) -> Result<U256, String> {
    let stripped = s.trim_start_matches("0x");
    if stripped.is_empty() {
        return Ok(U256::zero());
    }
    // Left-pad to even-hex then to 64 chars, decode as big-endian.
    let padded = format!("{:0>64}", stripped);
    let bytes = hex::decode(&padded).map_err(|e| e.to_string())?;
    if bytes.len() > 32 {
        return Err(format!("U256 hex too long: {}", bytes.len()));
    }
    Ok(U256::from_big_endian(&bytes))
}

fn parse_h256(s: &str) -> Result<H256, String> {
    let stripped = s.trim_start_matches("0x");
    // Left-pad to 64 hex chars (32 bytes).
    let padded = format!("{:0>64}", stripped);
    let bytes = hex::decode(&padded).map_err(|e| e.to_string())?;
    if bytes.len() != 32 {
        return Err(format!("bad h256 length {}", bytes.len()));
    }
    Ok(H256::from_slice(&bytes))
}
