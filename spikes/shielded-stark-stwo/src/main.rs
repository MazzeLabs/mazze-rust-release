//! P0 spike, Stwo side: proof size / prove / verify time for 128 Poseidon2
//! permutations (≈ the Shielded Space 2×2 statement — see the Plonky3 spike's
//! README for the count) under Stwo's own security formula.
//!
//! Stwo v1.0.0 has no FRI folding-arity knob (binary folding only); `fold_step` is
//! accepted for symmetry and ignored. Stwo scores a config as `pow_bits + log_blowup · n_queries` (ethSTARK
//! conjectured). Its challenge field is QM31 (124 bits), hard-wired.

mod poseidon;

use std::time::Instant;

use stwo::core::air::Component;
use stwo::core::channel::Blake2sChannel;
use stwo::core::fri::FriConfig;
use stwo::core::pcs::{CommitmentSchemeVerifier, PcsConfig};
use stwo::core::vcs::blake2_merkle::Blake2sMerkleChannel;
use stwo::core::verifier::verify;

use poseidon::{prove_poseidon, PoseidonElements};

fn log_n_instances() -> u32 {
    std::env::var("LOG_N_INSTANCES").ok().and_then(|v| v.parse().ok()).unwrap_or(7)
}
const POW_BITS: u32 = 16;
const TARGET_BITS: u32 = 128;

fn run(log_blowup: u32, _fold_step: u32) {
    let n_queries = (TARGET_BITS - POW_BITS).div_ceil(log_blowup) as usize;
    let log_last: u32 = std::env::var("LOG_LAST").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let config = PcsConfig {
        pow_bits: POW_BITS,
        fri_config: FriConfig::new(log_last, log_blowup, n_queries),
    };

    let t = Instant::now();
    let (component, proof) = prove_poseidon(log_n_instances(), config);
    let prove_ms = t.elapsed().as_secs_f64() * 1e3;

    let bytes = postcard::to_allocvec(&proof).expect("serialize").len();

    const VERIFY_RUNS: u32 = 20;
    let t = Instant::now();
    for _ in 0..VERIFY_RUNS {
        let proof = proof.clone();
        let channel = &mut Blake2sChannel::default();
        let scheme = &mut CommitmentSchemeVerifier::<Blake2sMerkleChannel>::new(proof.config);
        let sizes = component.trace_log_degree_bounds();
        scheme.commit(proof.commitments[0], &sizes[0], channel);
        scheme.commit(proof.commitments[1], &sizes[1], channel);
        let lookup_elements = PoseidonElements::draw(channel);
        assert_eq!(lookup_elements, component.lookup_elements);
        scheme.commit(proof.commitments[2], &sizes[2], channel);
        verify(&[&component], channel, scheme, proof).expect("verify");
    }
    let verify_ms = t.elapsed().as_secs_f64() * 1e3 / VERIFY_RUNS as f64;

    println!(
        "| {:>6} | {:>5} | {:>7} | {:>4} | {:>8.1} | {:>8.1} | {:>9.2} |",
        1u32 << log_blowup,
        2,
        n_queries,
        config.security_bits(),
        bytes as f64 / 1024.0,
        prove_ms,
        verify_ms
    );
}

fn main() {
    println!("| blowup | arity | queries | bits | proof KB | prove ms | verify ms |");
    println!("|-------:|------:|--------:|-----:|---------:|---------:|----------:|");
    for &(log_blowup, fold_step) in &sweep() {
        run(log_blowup, fold_step);
    }
}

/// `SWEEP="1:1,2:1"` → [(log_blowup, fold_step)].
fn sweep() -> Vec<(u32, u32)> {
    std::env::var("SWEEP")
        .unwrap_or_else(|_| "1:1".into())
        .split(',')
        .map(|p| {
            let (b, f) = p.split_once(':').expect("blowup:fold");
            (b.parse().unwrap(), f.parse().unwrap())
        })
        .collect()
}
