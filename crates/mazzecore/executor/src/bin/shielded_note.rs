// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use ark_std::rand::{rngs::OsRng, rngs::StdRng, RngCore, SeedableRng};
use mazze_addr::{mazze_addr_decode, mazze_addr_encode, EncodingOptions, Network};
use mazze_parameters::consensus::ONE_MAZZE_IN_MAZZY;
use mazze_types::{H256, U256};
use mazzekey::{crypto::ecies, KeyPair, Public, Secret};
use mazze_executor::shielded::{
    fr_from_bytes, fr_from_h256, fr_from_u256, fr_to_h256, poseidon_hash,
    poseidon_hash2, split_recipient,
};
use rustc_hex::{FromHex, ToHex};
use serde_json::json;

const NOTE_VERSION: u8 = 1;
const MERKLE_DEPTH_DEFAULT: usize = 32;

fn to_network(id: u64) -> Network {
    match id {
        1990 => Network::Main,
        1 => Network::Test,
        other => Network::Id(other),
    }
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

fn parse_shielded_address(input: &str) -> Result<[u8; 64], String> {
    let trimmed = input.trim();
    if trimmed.contains(':') {
        let decoded = mazze_addr_decode(trimmed)
            .map_err(|e| format!("invalid base32 address {}: {:?}", input, e))?;
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

fn u256_to_be_bytes(value: &U256) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    value.to_big_endian(&mut bytes);
    bytes
}

fn build_commitment(
    recipient: &[u8; 64], value: &U256, rho: &[u8; 32], rseed: &[u8; 32],
) -> H256 {
    let (rcpt_left, rcpt_right) = split_recipient(recipient);
    let value_fr = fr_from_u256(value);
    let rho_fr = fr_from_bytes(rho);
    let rseed_fr = fr_from_bytes(rseed);
    let commitment = poseidon_hash(&[
        rcpt_left,
        rcpt_right,
        value_fr,
        rho_fr,
        rseed_fr,
    ]);
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

fn rng_from_seed(seed: Option<u64>) -> StdRng {
    match seed {
        Some(seed) => StdRng::seed_from_u64(seed),
        None => {
            let mut seed_bytes = [0u8; 32];
            OsRng.fill_bytes(&mut seed_bytes);
            StdRng::from_seed(seed_bytes)
        }
    }
}

fn merkle_zeroes(depth: usize) -> Vec<H256> {
    let mut zeros = Vec::with_capacity(depth + 1);
    zeros.push(H256::zero());
    for level in 0..depth {
        let left = fr_from_h256(&zeros[level]);
        let right = fr_from_h256(&zeros[level]);
        let next = fr_to_h256(&poseidon_hash2(&left, &right));
        zeros.push(next);
    }
    zeros
}

fn merkle_path(
    commitments: &[H256], depth: usize, index: usize,
) -> Result<(H256, Vec<H256>, Vec<bool>), String> {
    let mut level = commitments.to_vec();
    let zeroes = merkle_zeroes(depth);
    let mut idx = index;
    let mut elements = Vec::with_capacity(depth);
    let mut bits = Vec::with_capacity(depth);

    for d in 0..depth {
        let is_right = (idx & 1) == 1;
        let sibling = if is_right {
            if idx == 0 {
                zeroes[d]
            } else {
                level.get(idx - 1).copied().unwrap_or(zeroes[d])
            }
        } else {
            level.get(idx + 1).copied().unwrap_or(zeroes[d])
        };
        elements.push(sibling);
        bits.push(is_right);

        let mut next = Vec::with_capacity((level.len() + 1) / 2);
        let mut j = 0usize;
        while j < level.len() {
            let left = level[j];
            let right = level.get(j + 1).copied().unwrap_or(zeroes[d]);
            let left_fr = fr_from_h256(&left);
            let right_fr = fr_from_h256(&right);
            next.push(fr_to_h256(&poseidon_hash2(&left_fr, &right_fr)));
            j += 2;
        }
        level = next;
        idx >>= 1;
    }

    let root = if level.is_empty() {
        zeroes[depth]
    } else {
        level[0]
    };
    Ok((root, elements, bits))
}

fn cmd_address(args: &[String]) -> Result<(), String> {
    let mut secret = None;
    let mut public = None;
    let mut network_id = 1990u64;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--secret" => {
                i += 1;
                secret = args.get(i).cloned();
            }
            "--public" => {
                i += 1;
                public = args.get(i).cloned();
            }
            "--network-id" => {
                i += 1;
                network_id = args
                    .get(i)
                    .ok_or("missing --network-id")?
                    .parse::<u64>()
                    .map_err(|_| "invalid network id")?;
            }
            _ => return Err(format!("unknown arg: {}", args[i])),
        }
        i += 1;
    }

    let public_bytes = if let Some(secret) = secret {
        let secret: Secret = secret
            .parse()
            .map_err(|e| format!("invalid secret: {:?}", e))?;
        let keypair = KeyPair::from_secret(secret)
            .map_err(|e| format!("invalid secret: {:?}", e))?;
        let mut bytes = [0u8; 64];
        bytes.copy_from_slice(keypair.public().as_bytes());
        bytes
    } else if let Some(public) = public {
        parse_shielded_address(&public)?
    } else {
        return Err("missing --secret or --public".into());
    };

    let network = to_network(network_id);
    let addr = mazze_addr_encode(&public_bytes, network, EncodingOptions::Simple)
        .map_err(|e| format!("address encoding failed: {:?}", e))?;
    println!("{}", addr);
    Ok(())
}

fn cmd_build(args: &[String]) -> Result<(), String> {
    let mut to = None;
    let mut value = None;
    let mut value_mazze = None;
    let mut seed = None;
    let mut network_id = 1990u64;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
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
            "--seed" => {
                i += 1;
                seed = Some(
                    args.get(i)
                        .ok_or("missing --seed")?
                        .parse::<u64>()
                        .map_err(|_| "invalid seed")?,
                );
            }
            "--network-id" => {
                i += 1;
                network_id = args
                    .get(i)
                    .ok_or("missing --network-id")?
                    .parse::<u64>()
                    .map_err(|_| "invalid network id")?;
            }
            _ => return Err(format!("unknown arg: {}", args[i])),
        }
        i += 1;
    }

    let to = to.ok_or("missing --to")?;
    let recipient = parse_shielded_address(&to)?;

    let value = match (value, value_mazze) {
        (Some(raw), None) => raw,
        (None, Some(mazze)) => mazze * U256::from(ONE_MAZZE_IN_MAZZY),
        (Some(_), Some(_)) => return Err("use either --value or --value-mazze".into()),
        (None, None) => return Err("missing --value".into()),
    };

    let mut rng = rng_from_seed(seed);
    let mut rho = [0u8; 32];
    let mut rseed = [0u8; 32];
    rng.fill_bytes(&mut rho);
    rng.fill_bytes(&mut rseed);

    let commitment = build_commitment(&recipient, &value, &rho, &rseed);
    let plain = build_note_plain(&value, &rho, &rseed);
    let mut public = Public::default();
    public.as_bytes_mut().copy_from_slice(&recipient);
    let ciphertext = ecies::encrypt(&public, commitment.as_ref(), &plain)
        .map_err(|e| format!("note encryption failed: {:?}", e))?;

    let recipient_base32 = mazze_addr_encode(
        &recipient,
        to_network(network_id),
        EncodingOptions::Simple,
    )
    .map_err(|e| format!("address encoding failed: {:?}", e))?;

    let payload = json!({
        "recipient": recipient_base32,
        "commitment": format!("0x{}", commitment.as_ref().to_hex::<String>()),
        "ciphertext": format!("0x{}", ciphertext.to_hex::<String>()),
        "value": value.to_string(),
        "rho": format!("0x{}", rho.to_hex::<String>()),
        "rseed": format!("0x{}", rseed.to_hex::<String>())
    });
    println!("{}", payload);
    Ok(())
}

fn cmd_decrypt(args: &[String]) -> Result<(), String> {
    let mut secret = None;
    let mut commitment = None;
    let mut ciphertext = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--secret" => {
                i += 1;
                secret = args.get(i).cloned();
            }
            "--commitment" => {
                i += 1;
                commitment = args.get(i).cloned();
            }
            "--ciphertext" => {
                i += 1;
                ciphertext = args.get(i).cloned();
            }
            _ => return Err(format!("unknown arg: {}", args[i])),
        }
        i += 1;
    }

    let secret = secret.ok_or("missing --secret")?;
    let commitment = commitment.ok_or("missing --commitment")?;
    let ciphertext = ciphertext.ok_or("missing --ciphertext")?;

    let secret: Secret = secret
        .parse()
        .map_err(|e| format!("invalid secret: {:?}", e))?;
    let commitment = parse_h256(&commitment)?;
    let ciphertext = parse_hex_bytes(&ciphertext)?;

    let plain = ecies::decrypt(&secret, commitment.as_ref(), &ciphertext)
        .map_err(|e| format!("note decryption failed: {:?}", e))?;

    if plain.len() < 1 + 32 + 32 + 32 {
        return Err("invalid note length".into());
    }
    if plain[0] != NOTE_VERSION {
        return Err("unsupported note version".into());
    }
    let value = U256::from_big_endian(&plain[1..33]);
    let mut rho = [0u8; 32];
    let mut rseed = [0u8; 32];
    rho.copy_from_slice(&plain[33..65]);
    rseed.copy_from_slice(&plain[65..97]);

    let secret_fr = fr_from_bytes(secret.as_ref());
    let rho_fr = fr_from_bytes(&rho);
    let nullifier = fr_to_h256(&poseidon_hash(&[secret_fr, rho_fr]));

    let payload = json!({
        "value": value.to_string(),
        "rho": format!("0x{}", rho.to_hex::<String>()),
        "rseed": format!("0x{}", rseed.to_hex::<String>()),
        "nullifier": format!("0x{}", nullifier.as_ref().to_hex::<String>())
    });
    println!("{}", payload);
    Ok(())
}

fn cmd_path(args: &[String]) -> Result<(), String> {
    let mut commitments_path = None;
    let mut index = None;
    let mut depth = MERKLE_DEPTH_DEFAULT;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--commitments" => {
                i += 1;
                commitments_path = args.get(i).cloned();
            }
            "--index" => {
                i += 1;
                index = args.get(i).cloned();
            }
            "--depth" => {
                i += 1;
                depth = args
                    .get(i)
                    .ok_or("missing --depth")?
                    .parse::<usize>()
                    .map_err(|_| "invalid depth")?;
            }
            _ => return Err(format!("unknown arg: {}", args[i])),
        }
        i += 1;
    }

    let commitments_path = commitments_path.ok_or("missing --commitments")?;
    let index = index
        .ok_or("missing --index")?
        .parse::<usize>()
        .map_err(|_| "invalid index")?;

    let raw = std::fs::read_to_string(&commitments_path)
        .map_err(|e| format!("failed to read {}: {}", commitments_path, e))?;
    let mut commitments = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let first = trimmed.split_whitespace().next().unwrap_or("");
        if first.is_empty() {
            continue;
        }
        commitments.push(parse_h256(first)?);
    }

    if index >= commitments.len() {
        return Err("index out of range".into());
    }

    let (root, elements, bits) = merkle_path(&commitments, depth, index)?;
    let payload = json!({
        "root": format!("0x{}", root.as_ref().to_hex::<String>()),
        "elements": elements
            .into_iter()
            .map(|val| format!("0x{}", val.as_ref().to_hex::<String>()))
            .collect::<Vec<_>>(),
        "bits": bits.into_iter().map(|b| if b { 1 } else { 0 }).collect::<Vec<_>>()
    });
    println!("{}", payload);
    Ok(())
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() {
        return Err("usage: shielded_note <address|build|decrypt|path> [args]".into());
    }
    let cmd = args.remove(0);
    match cmd.as_str() {
        "address" => cmd_address(&args),
        "build" => cmd_build(&args),
        "decrypt" => cmd_decrypt(&args),
        "path" => cmd_path(&args),
        _ => Err("usage: shielded_note <address|build|decrypt|path> [args]".into()),
    }
}
