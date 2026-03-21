// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use ark_bls12_381::Bls12_381;
use ark_groth16::ProvingKey;
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use keccak_hash::keccak;
use rustc_hex::{FromHex, ToHex};
use std::{env, fs, path::PathBuf};

fn resolve_path(flag: &str, fallback_env: &str) -> Option<PathBuf> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut i = 0;
    let mut path = None;
    while i < args.len() {
        if args[i] == flag {
            i += 1;
            if let Some(value) = args.get(i) {
                path = Some(PathBuf::from(value));
            }
        }
        i += 1;
    }
    if path.is_none() {
        if let Ok(value) = env::var(fallback_env) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                path = Some(PathBuf::from(trimmed));
            }
        }
    }
    path
}

fn resolve_vk_path() -> Result<PathBuf, String> {
    let path = resolve_path("--vk", "MAZZE_SHIELDED_VK_HEX")
        .unwrap_or_else(|| PathBuf::from("run/shielded_vk.hex"));
    Ok(path)
}

fn resolve_pk_path() -> Option<PathBuf> {
    resolve_path("--pk", "MAZZE_SHIELDED_PK_HEX")
}

fn main() -> Result<(), String> {
    if let Some(path) = resolve_pk_path() {
        let content = fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Err(format!("proving key is empty: {}", path.display()));
        }
        let hex = trimmed.strip_prefix("0x").unwrap_or(trimmed);
        let bytes: Vec<u8> = hex.from_hex().map_err(|e| {
            format!("invalid hex in {}: {:?}", path.display(), e)
        })?;
        let pk = ProvingKey::<Bls12_381>::deserialize_compressed(&*bytes)
            .map_err(|e| {
                format!("failed to deserialize proving key: {:?}", e)
            })?;
        let mut vk_bytes = Vec::new();
        pk.vk.serialize_compressed(&mut vk_bytes).map_err(|e| {
            format!("failed to serialize verifying key: {:?}", e)
        })?;
        let hash = keccak(vk_bytes.as_slice());
        println!("0x{}", hash.as_ref().to_hex::<String>());
        return Ok(());
    }

    let path = resolve_vk_path()?;
    let content = fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(format!("verifying key is empty: {}", path.display()));
    }
    let hex = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    let bytes: Vec<u8> = hex
        .from_hex()
        .map_err(|e| format!("invalid hex in {}: {:?}", path.display(), e))?;
    let hash = keccak(bytes.as_slice());
    println!("0x{}", hash.as_ref().to_hex::<String>());
    Ok(())
}
