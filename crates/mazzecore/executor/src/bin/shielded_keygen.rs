// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use ark_bls12_381::Bls12_381;
use ark_groth16::Groth16;
use ark_snark::SNARK;
use ark_serialize::CanonicalSerialize;
use ark_std::rand::{rngs::StdRng, SeedableRng};
use mazze_executor::shielded::circuit::ShieldedCircuit;
use rustc_hex::ToHex;
use std::{env, fs, path::PathBuf};

fn parse_args() -> (Option<PathBuf>, Option<PathBuf>, u64) {
    let mut out_vk = None;
    let mut out_pk = None;
    let mut seed = 0u64;
    let args = env::args().skip(1).collect::<Vec<_>>();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" | "--out-vk" => {
                i += 1;
                if i < args.len() {
                    out_vk = Some(PathBuf::from(&args[i]));
                }
            }
            "--out-pk" => {
                i += 1;
                if i < args.len() {
                    out_pk = Some(PathBuf::from(&args[i]));
                }
            }
            "--seed" => {
                i += 1;
                if i < args.len() {
                    seed = args[i].parse().unwrap_or(seed);
                }
            }
            _ => {}
        }
        i += 1;
    }

    if out_vk.is_none() {
        out_vk = Some(PathBuf::from("run/shielded_vk.hex"));
    }
    if out_pk.is_none() {
        out_pk = Some(PathBuf::from("run/shielded_pk.hex"));
    }

    (out_vk, out_pk, seed)
}

fn main() {
    let (out_vk, out_pk, seed) = parse_args();

    let mut rng = StdRng::seed_from_u64(seed);
    let circuit = ShieldedCircuit::blank();

    let (pk, vk) =
        Groth16::<Bls12_381>::circuit_specific_setup(circuit, &mut rng)
            .expect("failed to generate verifying key");

    let mut vk_bytes = Vec::new();
    vk.serialize_compressed(&mut vk_bytes)
        .expect("failed to serialize verifying key");

    let vk_hex = vk_bytes.to_hex::<String>();

    if let Some(path) = out_vk {
        fs::write(&path, format!("{}\n", vk_hex))
            .expect("failed to write verifying key");
        println!("wrote verifying key to {}", path.display());
    }

    if let Some(path) = out_pk {
        let mut pk_bytes = Vec::new();
        pk.serialize_compressed(&mut pk_bytes)
            .expect("failed to serialize proving key");
        let pk_hex = pk_bytes.to_hex::<String>();
        fs::write(&path, format!("{}\n", pk_hex))
            .expect("failed to write proving key");
        println!("wrote proving key to {}", path.display());
    }

    println!("{}", vk_hex);
}
