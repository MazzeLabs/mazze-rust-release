// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use ark_bls12_381::{Bls12_381, Fr};
use ark_ff::{PrimeField, Zero};
use ark_groth16::{prepare_verifying_key, Groth16};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use ark_snark::SNARK;
use ark_std::rand::{rngs::StdRng, RngCore, SeedableRng};
use mazze_addr::mazze_addr_decode;
use mazze_executor::shielded::circuit::{
    ShieldedCircuit, ShieldedInputWitness, ShieldedOutputWitness, MERKLE_DEPTH,
};
use mazze_executor::shielded::{
    fr_from_bytes, fr_from_h256 as shielded_fr_from_h256,
    fr_from_u256 as shielded_fr_from_u256, fr_to_h256, poseidon_hash,
    split_recipient,
};
use mazze_parameters::consensus::ONE_MAZZE_IN_MAZZY;
use mazze_parameters::internal_contract_addresses::SHIELDED_POOL_CONTRACT_ADDRESS;
use mazze_types::{Address, H256, U256};
use mazzekey::crypto::ecies;
use mazzekey::Public;
use primitives::{
    transaction::{
        native_transaction::ShieldedTransaction, TypedNativeTransaction,
    },
    Action, Transaction, TransactionWithSignature,
};
use rustc_hex::{FromHex, ToHex};
use serde::Deserialize;
use sha3_macro::keccak;
use solidity_abi::ABIListWriter;
use std::str::FromStr;
use std::{env, fs};

const MAX_NULLIFIERS: usize = 8;
const MAX_COMMITMENTS: usize = 8;
const MAX_TRANSPARENT_OUTPUTS: usize = 8;
const NOTE_VERSION: u8 = 1;
const PUBLIC_INPUT_LEN: usize = 1
    + 3
    + MAX_NULLIFIERS
    + MAX_COMMITMENTS
    + MAX_TRANSPARENT_OUTPUTS
    + MAX_TRANSPARENT_OUTPUTS
    + 1;

#[derive(Clone)]
struct NoteSecret {
    recipient: [u8; 64],
    value: U256,
    rho: [u8; 32],
    rseed: [u8; 32],
}

#[derive(Deserialize)]
struct InputsFile {
    inputs: Vec<InputEntry>,
}

#[derive(Deserialize)]
struct InputEntry {
    recipient: String,
    value: String,
    rho: String,
    rseed: String,
    secret: String,
    path: InputPath,
    #[serde(default)]
    commitment: Option<String>,
}

#[derive(Deserialize)]
struct InputPath {
    elements: Vec<String>,
    bits: Vec<u8>,
}

fn load_hex_file(path: &str) -> Result<Option<Vec<u8>>, String> {
    if path.is_empty() {
        return Ok(None);
    }
    let content = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None)
        }
        Err(err) => return Err(format!("failed to read {}: {}", path, err)),
    };
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let hex = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    let bytes = hex
        .from_hex()
        .map_err(|e| format!("invalid hex in {}: {:?}", path, e))?;
    Ok(Some(bytes))
}

fn load_proving_key(
    path: &str,
) -> Result<Option<ark_groth16::ProvingKey<Bls12_381>>, String> {
    let Some(bytes) = load_hex_file(path)? else {
        return Ok(None);
    };
    let pk =
        ark_groth16::ProvingKey::<Bls12_381>::deserialize_compressed(&*bytes)
            .map_err(|e| {
            format!("failed to deserialize proving key {}: {:?}", path, e)
        })?;
    Ok(Some(pk))
}

fn parse_u256(input: &str) -> Result<U256, String> {
    let trimmed = input.trim();
    if let Some(hex) = trimmed.strip_prefix("0x") {
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

fn parse_hex_bytes(input: &str) -> Result<Vec<u8>, String> {
    let trimmed = input.trim();
    let hex = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    if hex.len() % 2 != 0 {
        return Err(format!("hex string must be even length: {}", input));
    }
    hex.from_hex()
        .map_err(|e| format!("invalid hex bytes {}: {:?}", input, e))
}

fn parse_hex_32(input: &str) -> Result<[u8; 32], String> {
    let bytes = parse_hex_bytes(input)?;
    if bytes.len() != 32 {
        return Err(format!("expected 32-byte hex: {}", input));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn parse_address(input: &str) -> Result<Address, String> {
    let trimmed = input.trim();
    if trimmed.contains(':') {
        let decoded = mazze_addr_decode(trimmed).map_err(|e| {
            format!("invalid base32 address {}: {:?}", input, e)
        })?;
        if let Some(addr) = decoded.hex_address {
            return Ok(addr);
        }
        return Err(format!("address is not 20 bytes: {}", input));
    }
    let hex = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    Address::from_str(hex)
        .map_err(|e| format!("invalid address {}: {:?}", input, e))
}

fn parse_shielded_address(input: &str) -> Result<[u8; 64], String> {
    let trimmed = input.trim();
    if trimmed.contains(':') {
        let decoded = mazze_addr_decode(trimmed).map_err(|e| {
            format!("invalid base32 address {}: {:?}", input, e)
        })?;
        if decoded.parsed_address_bytes.len() != 64 {
            return Err(format!(
                "shielded address must be 64 bytes: {}",
                input
            ));
        }
        let bytes: [u8; 64] = decoded.parsed_address_bytes[..]
            .try_into()
            .map_err(|_| "shielded address length mismatch".to_string())?;
        return Ok(bytes);
    }
    let bytes = parse_hex_bytes(trimmed)?;
    if bytes.len() != 64 {
        return Err(format!("shielded address must be 64 bytes: {}", input));
    }
    let bytes: [u8; 64] = bytes
        .try_into()
        .map_err(|_| "shielded address length mismatch".to_string())?;
    Ok(bytes)
}

fn parse_list<T>(
    input: Option<String>, parser: fn(&str) -> Result<T, String>,
) -> Result<Vec<T>, String> {
    let mut out = Vec::new();
    let Some(value) = input else {
        return Ok(out);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(out);
    }
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        out.push(parser(item)?);
    }
    Ok(out)
}

fn parse_inputs_file(path: &str) -> Result<Vec<InputEntry>, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("failed to read inputs file {}: {}", path, e))?;
    if let Ok(list) = serde_json::from_str::<Vec<InputEntry>>(&raw) {
        return Ok(list);
    }
    if let Ok(wrapper) = serde_json::from_str::<InputsFile>(&raw) {
        return Ok(wrapper.inputs);
    }
    Err("invalid inputs JSON".into())
}

fn build_public_inputs(
    anchor: &H256, nullifiers: &[H256], commitments: &[H256],
    outputs: &[Address], values: &[U256], fee: &U256,
) -> Vec<Fr> {
    let mut inputs = Vec::with_capacity(PUBLIC_INPUT_LEN);
    inputs.push(shielded_fr_from_h256(anchor));
    inputs.push(Fr::from(nullifiers.len() as u64));
    inputs.push(Fr::from(commitments.len() as u64));
    inputs.push(Fr::from(outputs.len() as u64));

    for idx in 0..MAX_NULLIFIERS {
        let value = nullifiers.get(idx).copied().unwrap_or_else(H256::zero);
        inputs.push(shielded_fr_from_h256(&value));
    }
    for idx in 0..MAX_COMMITMENTS {
        let value = commitments.get(idx).copied().unwrap_or_else(H256::zero);
        inputs.push(shielded_fr_from_h256(&value));
    }
    for idx in 0..MAX_TRANSPARENT_OUTPUTS {
        let value = outputs.get(idx).copied().unwrap_or_else(Address::zero);
        inputs.push(fr_from_address(&value));
    }
    for idx in 0..MAX_TRANSPARENT_OUTPUTS {
        let value = values.get(idx).copied().unwrap_or_else(U256::zero);
        inputs.push(shielded_fr_from_u256(&value));
    }

    inputs.push(shielded_fr_from_u256(fee));
    inputs
}

fn fr_from_address(value: &Address) -> Fr {
    Fr::from_be_bytes_mod_order(value.as_ref())
}

fn u256_to_be_bytes(value: &U256) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    value.to_big_endian(&mut bytes);
    bytes
}

fn build_commitment(
    recipient: &[u8; 64], value: &U256, rho: &[u8; 32], rseed: &[u8; 32],
) -> H256 {
    let (rcpt_left, rcpt_right) = split_recipient(recipient);
    let value_fr = shielded_fr_from_u256(value);
    let rho_fr = fr_from_bytes(rho);
    let rseed_fr = fr_from_bytes(rseed);
    let commitment =
        poseidon_hash(&[rcpt_left, rcpt_right, value_fr, rho_fr, rseed_fr]);
    fr_to_h256(&commitment)
}

fn build_note_plain(value: &U256, rho: &[u8; 32], rseed: &[u8; 32]) -> Vec<u8> {
    let mut note = Vec::with_capacity(1 + 32 + 32 + 32);
    note.push(NOTE_VERSION);
    note.extend_from_slice(&u256_to_be_bytes(value));
    note.extend_from_slice(rho);
    note.extend_from_slice(rseed);
    note
}

fn build_shielded_notes(
    rng: &mut StdRng, outputs: &[[u8; 64]], values: &[U256],
) -> Result<(Vec<H256>, Vec<Vec<u8>>, Vec<NoteSecret>), String> {
    let mut commitments = Vec::with_capacity(outputs.len());
    let mut ciphertexts = Vec::with_capacity(outputs.len());
    let mut secrets = Vec::with_capacity(outputs.len());
    for (output, value) in outputs.iter().zip(values.iter()) {
        let mut rho = [0u8; 32];
        let mut rseed = [0u8; 32];
        rng.fill_bytes(&mut rho);
        rng.fill_bytes(&mut rseed);
        let commitment = build_commitment(output, value, &rho, &rseed);
        let plain = build_note_plain(value, &rho, &rseed);
        let mut public = Public::default();
        public.as_bytes_mut().copy_from_slice(output);
        let ciphertext =
            ecies::encrypt(&public, commitment.as_ref(), &plain)
                .map_err(|e| format!("note encryption failed: {:?}", e))?;
        commitments.push(commitment);
        ciphertexts.push(ciphertext);
        secrets.push(NoteSecret {
            recipient: *output,
            value: *value,
            rho,
            rseed,
        });
    }
    Ok((commitments, ciphertexts, secrets))
}

fn main() -> Result<(), String> {
    let mut anchor = H256::zero();
    let mut nullifiers = None;
    let mut commitments = None;
    let mut ciphertexts = None;
    let mut outputs = None;
    let mut values_raw = None;
    let mut values_mazze = None;
    let mut shielded_outputs = None;
    let mut shielded_values_raw = None;
    let mut shielded_values_mazze = None;
    let mut fee = None;
    let mut fee_mazze = None;
    let mut gas = U256::from(5_000_000u64);
    let mut nonce = U256::zero();
    let mut storage_limit = 0u64;
    let mut chain_id = 0u32;
    let mut epoch_height = 0u64;
    let mut seed = 0u64;
    let mut inputs_path = None;

    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--anchor" => {
                i += 1;
                anchor = parse_h256(args.get(i).ok_or("missing --anchor")?)?;
            }
            "--nullifiers" => {
                i += 1;
                nullifiers = args.get(i).cloned();
            }
            "--commitments" => {
                i += 1;
                commitments = args.get(i).cloned();
            }
            "--ciphertexts" => {
                i += 1;
                ciphertexts = args.get(i).cloned();
            }
            "--outputs" => {
                i += 1;
                outputs = args.get(i).cloned();
            }
            "--values" => {
                i += 1;
                values_raw = args.get(i).cloned();
            }
            "--values-mazze" => {
                i += 1;
                values_mazze = args.get(i).cloned();
            }
            "--shielded-outputs" => {
                i += 1;
                shielded_outputs = args.get(i).cloned();
            }
            "--shielded-values" => {
                i += 1;
                shielded_values_raw = args.get(i).cloned();
            }
            "--shielded-values-mazze" => {
                i += 1;
                shielded_values_mazze = args.get(i).cloned();
            }
            "--fee" => {
                i += 1;
                fee = Some(parse_u256(args.get(i).ok_or("missing --fee")?)?);
            }
            "--fee-mazze" => {
                i += 1;
                fee_mazze = Some(parse_u256(
                    args.get(i).ok_or("missing --fee-mazze")?,
                )?);
            }
            "--gas" => {
                i += 1;
                gas = parse_u256(args.get(i).ok_or("missing --gas")?)?;
            }
            "--nonce" => {
                i += 1;
                nonce = parse_u256(args.get(i).ok_or("missing --nonce")?)?;
            }
            "--storage-limit" => {
                i += 1;
                storage_limit =
                    parse_u64(args.get(i).ok_or("missing --storage-limit")?)?;
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
            "--seed" => {
                i += 1;
                seed = parse_u64(args.get(i).ok_or("missing --seed")?)?;
            }
            "--inputs" | "--inputs-json" => {
                i += 1;
                inputs_path = args.get(i).cloned();
            }
            _ => {
                return Err(format!("unknown arg: {}", args[i]));
            }
        }
        i += 1;
    }

    let mut input_witnesses = Vec::new();
    let mut input_nullifiers = Vec::new();
    if let Some(path) = inputs_path.as_deref() {
        let entries = parse_inputs_file(path)?;
        if entries.len() > MAX_NULLIFIERS {
            return Err("too many input notes".into());
        }
        for entry in entries {
            let recipient = parse_shielded_address(&entry.recipient)?;
            let value = parse_u256(&entry.value)?;
            let rho = parse_hex_32(&entry.rho)?;
            let rseed = parse_hex_32(&entry.rseed)?;
            let secret = parse_hex_32(&entry.secret)?;
            let path_elements = entry
                .path
                .elements
                .iter()
                .map(|val| parse_h256(val))
                .collect::<Result<Vec<_>, _>>()?;
            let path_bits = entry
                .path
                .bits
                .iter()
                .map(|bit| match bit {
                    0 => Ok(false),
                    1 => Ok(true),
                    _ => Err("path bits must be 0 or 1".to_string()),
                })
                .collect::<Result<Vec<_>, _>>()?;
            if path_elements.len() != MERKLE_DEPTH
                || path_bits.len() != MERKLE_DEPTH
            {
                return Err("invalid merkle path length".into());
            }

            let commitment = build_commitment(&recipient, &value, &rho, &rseed);
            if let Some(expected) = entry.commitment.as_deref() {
                let expected = parse_h256(expected)?;
                if expected != commitment {
                    return Err("input commitment mismatch".into());
                }
            }

            let (rcpt_left, rcpt_right) = split_recipient(&recipient);
            let value_fr = shielded_fr_from_u256(&value);
            let rho_fr = fr_from_bytes(&rho);
            let rseed_fr = fr_from_bytes(&rseed);
            let secret_fr = fr_from_bytes(&secret);
            let nullifier = fr_to_h256(&poseidon_hash(&[secret_fr, rho_fr]));

            input_witnesses.push(ShieldedInputWitness {
                recipient_left: rcpt_left,
                recipient_right: rcpt_right,
                value: value_fr,
                rho: rho_fr,
                rseed: rseed_fr,
                secret: secret_fr,
                path_elements: path_elements
                    .iter()
                    .map(|val| shielded_fr_from_h256(val))
                    .collect(),
                path_bits,
            });
            input_nullifiers.push(nullifier);
        }
    }

    let nullifiers = if !input_nullifiers.is_empty() {
        input_nullifiers.clone()
    } else {
        parse_list(nullifiers, parse_h256)?
    };
    let outputs = parse_list(outputs, parse_address)?;
    let values = match (values_raw, values_mazze) {
        (Some(raw), None) => parse_list(Some(raw), parse_u256)?,
        (None, Some(mazze)) => {
            let mut values = Vec::new();
            for val in parse_list(Some(mazze), parse_u256)? {
                values.push(val * U256::from(ONE_MAZZE_IN_MAZZY));
            }
            values
        }
        (Some(_), Some(_)) => {
            return Err("use either --values or --values-mazze".into())
        }
        (None, None) => Vec::new(),
    };

    let shielded_outputs =
        parse_list(shielded_outputs, parse_shielded_address)?;
    let shielded_values = match (shielded_values_raw, shielded_values_mazze) {
        (Some(raw), None) => parse_list(Some(raw), parse_u256)?,
        (None, Some(mazze)) => {
            let mut values = Vec::new();
            for val in parse_list(Some(mazze), parse_u256)? {
                values.push(val * U256::from(ONE_MAZZE_IN_MAZZY));
            }
            values
        }
        (Some(_), Some(_)) => {
            return Err(
                "use either --shielded-values or --shielded-values-mazze"
                    .into(),
            )
        }
        (None, None) => Vec::new(),
    };

    let mut commitments = parse_list(commitments, parse_h256)?;
    let mut ciphertexts = parse_list(ciphertexts, parse_hex_bytes)?;

    if outputs.len() != values.len() {
        return Err("outputs length must match values length".into());
    }
    if nullifiers.len() > MAX_NULLIFIERS {
        return Err("too many nullifiers".into());
    }
    if outputs.len() > MAX_TRANSPARENT_OUTPUTS {
        return Err("too many outputs".into());
    }
    if shielded_outputs.len() != shielded_values.len() {
        return Err("shielded outputs length must match values length".into());
    }
    if shielded_outputs.len() > MAX_COMMITMENTS {
        return Err("too many shielded outputs".into());
    }

    let mut output_witnesses = Vec::new();
    if !shielded_outputs.is_empty() || !shielded_values.is_empty() {
        if !commitments.is_empty() || !ciphertexts.is_empty() {
            return Err(
                "use either --shielded-outputs or --commitments/--ciphertexts"
                    .into(),
            );
        }
        let mut note_rng = StdRng::seed_from_u64(seed ^ 0x9e3779b97f4a7c15);
        let (note_commitments, note_ciphertexts, note_secrets) =
            build_shielded_notes(
                &mut note_rng,
                &shielded_outputs,
                &shielded_values,
            )?;
        commitments = note_commitments;
        ciphertexts = note_ciphertexts;
        output_witnesses = note_secrets
            .into_iter()
            .map(|note| {
                let (left, right) = split_recipient(&note.recipient);
                ShieldedOutputWitness {
                    recipient_left: left,
                    recipient_right: right,
                    value: shielded_fr_from_u256(&note.value),
                    rho: fr_from_bytes(&note.rho),
                    rseed: fr_from_bytes(&note.rseed),
                }
            })
            .collect();
    } else if commitments.len() != ciphertexts.len() {
        return Err("commitments length must match ciphertexts length".into());
    }
    if commitments.len() > MAX_COMMITMENTS {
        return Err("too many commitments".into());
    }
    if !commitments.is_empty() && output_witnesses.is_empty() {
        return Err("shielded outputs required to build proof".into());
    }

    let fee = match (fee, fee_mazze) {
        (Some(raw), None) => raw,
        (None, Some(mazze)) => mazze * U256::from(ONE_MAZZE_IN_MAZZY),
        (Some(_), Some(_)) => {
            return Err("use either --fee or --fee-mazze".into())
        }
        (None, None) => U256::zero(),
    };

    if input_witnesses.is_empty()
        && (!commitments.is_empty() || !values.is_empty() || !fee.is_zero())
    {
        return Err("inputs file required for shielded spends".into());
    }
    if nullifiers.len() > 0 && input_witnesses.is_empty() {
        return Err("inputs file required for non-empty nullifiers".into());
    }
    if !input_witnesses.is_empty() && input_witnesses.len() != nullifiers.len()
    {
        return Err("input witness length mismatch".into());
    }

    let public_inputs = build_public_inputs(
        &anchor,
        &nullifiers,
        &commitments,
        &outputs,
        &values,
        &fee,
    );
    if public_inputs.len() != PUBLIC_INPUT_LEN {
        return Err("public input length mismatch".into());
    }

    let mut nullifiers_fr = nullifiers
        .iter()
        .map(shielded_fr_from_h256)
        .collect::<Vec<_>>();
    let mut commitments_fr = commitments
        .iter()
        .map(shielded_fr_from_h256)
        .collect::<Vec<_>>();
    let mut outputs_fr =
        outputs.iter().map(fr_from_address).collect::<Vec<_>>();
    let mut values_fr = values
        .iter()
        .map(|v| shielded_fr_from_u256(v))
        .collect::<Vec<_>>();
    while nullifiers_fr.len() < MAX_NULLIFIERS {
        nullifiers_fr.push(Fr::zero());
    }
    while commitments_fr.len() < MAX_COMMITMENTS {
        commitments_fr.push(Fr::zero());
    }
    while outputs_fr.len() < MAX_TRANSPARENT_OUTPUTS {
        outputs_fr.push(Fr::zero());
    }
    while values_fr.len() < MAX_TRANSPARENT_OUTPUTS {
        values_fr.push(Fr::zero());
    }

    while input_witnesses.len() < MAX_NULLIFIERS {
        input_witnesses.push(ShieldedInputWitness {
            recipient_left: Fr::zero(),
            recipient_right: Fr::zero(),
            value: Fr::zero(),
            rho: Fr::zero(),
            rseed: Fr::zero(),
            secret: Fr::zero(),
            path_elements: vec![Fr::zero(); MERKLE_DEPTH],
            path_bits: vec![false; MERKLE_DEPTH],
        });
    }
    while output_witnesses.len() < MAX_COMMITMENTS {
        output_witnesses.push(ShieldedOutputWitness {
            recipient_left: Fr::zero(),
            recipient_right: Fr::zero(),
            value: Fr::zero(),
            rho: Fr::zero(),
            rseed: Fr::zero(),
        });
    }

    let mut rng = StdRng::seed_from_u64(seed);
    let pk_path = env::var("SHIELDED_PK_HEX")
        .unwrap_or_else(|_| "run/shielded_pk.hex".to_string());
    let allow_ephemeral =
        env::var("SHIELDED_ALLOW_EPHEMERAL_PK").unwrap_or_default() == "1";
    let pk = match load_proving_key(&pk_path)? {
        Some(pk) => pk,
        None if allow_ephemeral => {
            let setup_circuit = ShieldedCircuit::blank();
            let (pk, _vk) = Groth16::<Bls12_381>::circuit_specific_setup(
                setup_circuit,
                &mut rng,
            )
            .map_err(|e| format!("setup failed: {:?}", e))?;
            pk
        }
        None => {
            return Err(format!(
                "missing proving key at {}; generate with shielded_keygen and ensure the on-chain VK matches",
                pk_path
            ));
        }
    };

    let circuit = ShieldedCircuit {
        anchor: shielded_fr_from_h256(&anchor),
        num_inputs: nullifiers.len() as u64,
        num_commitments: commitments.len() as u64,
        num_outputs: outputs.len() as u64,
        nullifiers: nullifiers_fr,
        commitments: commitments_fr,
        transparent_outputs: outputs_fr,
        transparent_values: values_fr,
        fee: shielded_fr_from_u256(&fee),
        inputs: input_witnesses,
        outputs: output_witnesses,
    };

    let mut proof = Groth16::<Bls12_381>::prove(&pk, circuit.clone(), &mut rng)
        .map_err(|e| format!("prove failed: {:?}", e))?;
    let pvk = prepare_verifying_key(&pk.vk);
    if !Groth16::<Bls12_381>::verify_proof(&pvk, &proof, &public_inputs)
        .unwrap_or(false)
    {
        let mut retry_rng = StdRng::seed_from_u64(seed.wrapping_add(1));
        let retry_proof =
            Groth16::<Bls12_381>::prove(&pk, circuit.clone(), &mut retry_rng)
                .map_err(|e| format!("prove retry failed: {:?}", e))?;
        if !Groth16::<Bls12_381>::verify_proof(
            &pvk,
            &retry_proof,
            &public_inputs,
        )
        .unwrap_or(false)
        {
            return Err("proof verification failed".into());
        }
        proof = retry_proof;
    }
    let mut proof_bytes = Vec::new();
    proof
        .serialize_compressed(&mut proof_bytes)
        .map_err(|e| format!("proof serialize failed: {:?}", e))?;

    let mut writer = ABIListWriter::with_heads_length(8 * 32);
    writer.write_down(&anchor);
    writer.write_down(&nullifiers);
    writer.write_down(&commitments);
    writer.write_down(&ciphertexts);
    writer.write_down(&outputs);
    writer.write_down(&values);
    writer.write_down(&fee);
    writer.write_down(&proof_bytes);
    let encoded = writer.into_linked_bytes();
    let mut data = Vec::with_capacity(4 + encoded.len());
    let selector = keccak!(
        "applyShieldedBundle(bytes32,bytes32[],bytes32[],bytes[],address[],uint256[],uint256,bytes)"
    );
    data.extend_from_slice(&selector[0..4]);
    data.extend_from_slice(&encoded.to_vec());

    let shielded_tx = ShieldedTransaction {
        nonce,
        gas_price: U256::zero(),
        gas,
        action: Action::Call(SHIELDED_POOL_CONTRACT_ADDRESS),
        value: U256::zero(),
        storage_limit,
        epoch_height,
        chain_id,
        data: data.into(),
    };
    let unsigned =
        Transaction::Native(TypedNativeTransaction::Shielded(shielded_tx));
    let tx_with_sig = TransactionWithSignature::new_unsigned(unsigned);
    let raw = rlp::encode(&tx_with_sig);

    println!("0x{}", raw.to_hex::<String>());
    Ok(())
}
