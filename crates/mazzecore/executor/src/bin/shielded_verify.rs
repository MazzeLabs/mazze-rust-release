// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use ark_bls12_381::{Bls12_381, Fr};
use ark_ff::PrimeField;
use ark_groth16::{prepare_verifying_key, Groth16, Proof, VerifyingKey};
use ark_serialize::CanonicalDeserialize;
use mazze_types::{Address, H256, U256};
use rustc_hex::FromHex;
use solidity_abi::{read_abi_list, ABIDecodable, ABIDecodeError};
use std::{env, fs, path::PathBuf};

type Bytes = Vec<u8>;

const MAX_NULLIFIERS: usize = 8;
const MAX_COMMITMENTS: usize = 8;
const MAX_TRANSPARENT_OUTPUTS: usize = 8;
const PUBLIC_INPUT_LEN: usize = 1
    + 3
    + MAX_NULLIFIERS
    + MAX_COMMITMENTS
    + MAX_TRANSPARENT_OUTPUTS
    + MAX_TRANSPARENT_OUTPUTS
    + 1;

#[derive(Clone, Debug)]
struct ShieldedBundleInput {
    anchor: H256,
    nullifiers: Vec<H256>,
    commitments: Vec<H256>,
    ciphertexts: Vec<Bytes>,
    outputs: Vec<Address>,
    values: Vec<U256>,
    fee: U256,
    proof: Bytes,
}

impl ABIDecodable for ShieldedBundleInput {
    fn abi_decode(data: &[u8]) -> Result<Self, ABIDecodeError> {
        let mut pointer = data.iter();
        Ok(ShieldedBundleInput {
            anchor: read_abi_list::<H256>(data, &mut pointer)?,
            nullifiers: read_abi_list::<Vec<H256>>(data, &mut pointer)?,
            commitments: read_abi_list::<Vec<H256>>(data, &mut pointer)?,
            ciphertexts: read_abi_list::<Vec<Bytes>>(data, &mut pointer)?,
            outputs: read_abi_list::<Vec<Address>>(data, &mut pointer)?,
            values: read_abi_list::<Vec<U256>>(data, &mut pointer)?,
            fee: read_abi_list::<U256>(data, &mut pointer)?,
            proof: read_abi_list::<Bytes>(data, &mut pointer)?,
        })
    }
}

fn build_public_inputs(
    anchor: &H256, nullifiers: &[H256], commitments: &[H256],
    outputs: &[Address], values: &[U256], fee: &U256,
) -> Vec<Fr> {
    let mut inputs = Vec::with_capacity(PUBLIC_INPUT_LEN);
    inputs.push(fr_from_h256(anchor));
    inputs.push(Fr::from(nullifiers.len() as u64));
    inputs.push(Fr::from(commitments.len() as u64));
    inputs.push(Fr::from(outputs.len() as u64));

    for idx in 0..MAX_NULLIFIERS {
        let value = nullifiers.get(idx).copied().unwrap_or_else(H256::zero);
        inputs.push(fr_from_h256(&value));
    }
    for idx in 0..MAX_COMMITMENTS {
        let value = commitments.get(idx).copied().unwrap_or_else(H256::zero);
        inputs.push(fr_from_h256(&value));
    }
    for idx in 0..MAX_TRANSPARENT_OUTPUTS {
        let value = outputs.get(idx).copied().unwrap_or_else(Address::zero);
        inputs.push(fr_from_address(&value));
    }
    for idx in 0..MAX_TRANSPARENT_OUTPUTS {
        let value = values.get(idx).copied().unwrap_or_else(U256::zero);
        inputs.push(fr_from_u256(&value));
    }

    inputs.push(fr_from_u256(fee));
    inputs
}

fn fr_from_h256(value: &H256) -> Fr {
    Fr::from_be_bytes_mod_order(value.as_ref())
}

fn fr_from_address(value: &Address) -> Fr {
    Fr::from_be_bytes_mod_order(value.as_ref())
}

fn fr_from_u256(value: &U256) -> Fr {
    let mut bytes = [0u8; 32];
    value.to_big_endian(&mut bytes);
    Fr::from_be_bytes_mod_order(&bytes)
}

fn resolve_vk_path() -> PathBuf {
    let mut args = env::args().skip(1);
    let mut path = None;
    while let Some(arg) = args.next() {
        if arg == "--vk" {
            if let Some(value) = args.next() {
                path = Some(PathBuf::from(value));
            }
        }
    }
    if path.is_none() {
        if let Ok(value) = env::var("MAZZE_SHIELDED_VK_HEX") {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                path = Some(PathBuf::from(trimmed));
            }
        }
    }
    path.unwrap_or_else(|| PathBuf::from("run/shielded_vk.hex"))
}

fn main() -> Result<(), String> {
    let mut data_hex = None;
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut idx = 0;
    while idx < args.len() {
        if args[idx] == "--data" {
            idx += 1;
            if let Some(value) = args.get(idx) {
                data_hex = Some(value.clone());
            }
        }
        idx += 1;
    }
    let Some(mut data_hex) = data_hex else {
        return Err("usage: shielded_verify --data <hex> [--vk <path>]".into());
    };
    if let Some(trimmed) = data_hex.strip_prefix("0x") {
        data_hex = trimmed.to_string();
    }
    let data: Vec<u8> = data_hex
        .from_hex()
        .map_err(|e| format!("invalid hex data: {:?}", e))?;
    if data.len() < 4 {
        return Err("data too short".into());
    }
    let call_data = &data[4..];

    let bundle = ShieldedBundleInput::abi_decode(call_data)
        .map_err(|e| format!("abi decode failed: {:?}", e))?;

    let vk_path = resolve_vk_path();
    let vk_hex = fs::read_to_string(&vk_path)
        .map_err(|e| format!("failed to read {}: {}", vk_path.display(), e))?;
    let vk_hex = vk_hex.trim().strip_prefix("0x").unwrap_or(vk_hex.trim());
    let vk_bytes: Vec<u8> = vk_hex
        .from_hex()
        .map_err(|e| format!("invalid vk hex: {:?}", e))?;
    let vk = VerifyingKey::<Bls12_381>::deserialize_compressed(&*vk_bytes)
        .map_err(|e| format!("vk deserialize failed: {:?}", e))?;
    if vk.gamma_abc_g1.len() != PUBLIC_INPUT_LEN + 1 {
        return Err(format!(
            "vk public input mismatch: expected {}, got {}",
            PUBLIC_INPUT_LEN + 1,
            vk.gamma_abc_g1.len()
        ));
    }

    let _ = bundle.ciphertexts.len();
    let proof = Proof::<Bls12_381>::deserialize_compressed(&*bundle.proof)
        .map_err(|e| format!("proof deserialize failed: {:?}", e))?;
    let inputs = build_public_inputs(
        &bundle.anchor,
        &bundle.nullifiers,
        &bundle.commitments,
        &bundle.outputs,
        &bundle.values,
        &bundle.fee,
    );
    if inputs.len() != PUBLIC_INPUT_LEN {
        return Err(format!(
            "input length mismatch: expected {}, got {}",
            PUBLIC_INPUT_LEN,
            inputs.len()
        ));
    }
    let pvk = prepare_verifying_key(&vk);
    let ok = Groth16::<Bls12_381>::verify_proof(&pvk, &proof, &inputs)
        .unwrap_or(false);
    if ok {
        println!("ok");
    } else {
        println!("fail");
    }

    if ok {
        Ok(())
    } else {
        Err("invalid proof for provided inputs".into())
    }
}
