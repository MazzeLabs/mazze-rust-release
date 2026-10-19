//! P0 spike — how large and how fast is a STARK proof for the Shielded Space
//! 2-in × 2-out statement? See docs/internal/privacy-pq-stark.md §7, §11.
//!
//! The statement is dominated by Poseidon2 permutations:
//!   per input : ak, nk, owner, cm, nf (5) + 32 Merkle nodes  = 37
//!   per output: cm                                             = 1
//!   2×37 + 2×1 = 76, rounded up to the next power of two      = 128
//! Range checks (16-bit limbs) and the limb-wise balance add a few narrow
//! columns; they do not move the proof size meaningfully at this trace height.
//!
//! So this spike proves N Poseidon2 (KoalaBear, width 16) permutations with
//! Plonky3's stock AIR and reports proof size / prove / verify time for a
//! sweep of FRI parameters. It is a sizing estimate, not the real AIR.

use std::time::Instant;

use p3_challenger::{HashChallenger, SerializingChallenger32};
use p3_commit::ExtensionMmcs;
use p3_dft::Radix2DitParallel;
#[allow(unused_imports)]
use p3_field::extension::{BinomialExtensionField, QuinticTrinomialExtensionField};
#[allow(unused_imports)]
use p3_fri::{FriParameters, HidingFriPcs, TwoAdicFriPcs};
use p3_keccak::{Keccak256Hash, KeccakF};
use p3_koala_bear::{
    GenericPoseidon2LinearLayersKoalaBear, KoalaBear, KOALABEAR_POSEIDON2_HALF_FULL_ROUNDS,
    KOALABEAR_POSEIDON2_PARTIAL_ROUNDS_16, KOALABEAR_S_BOX_DEGREE,
};
#[allow(unused_imports)]
use p3_merkle_tree::{MerkleTreeHidingMmcs, MerkleTreeMmcs};
use p3_poseidon2_air::{RoundConstants, VectorizedPoseidon2Air};
use p3_symmetric::{CompressionFunctionFromHasher, PaddingFreeSponge, SerializingHasher};
use p3_field::coset::TwoAdicMultiplicativeCoset;
use p3_field::PrimeCharacteristicRing;
use p3_security::fri::FriRegime;
use p3_uni_stark::{
    prove, verify, AirLayout, ConjecturedSecurity, GrindingSites, OpeningShape, ProvenSecurity,
    StarkConfig, StarkSecurityParams,
};
use rand::rngs::StdRng;
use rand::SeedableRng;

const WIDTH: usize = 16;
const SBOX_DEGREE: u64 = KOALABEAR_S_BOX_DEGREE;
const SBOX_REGISTERS: usize = 0;
const HALF_FULL_ROUNDS: usize = KOALABEAR_POSEIDON2_HALF_FULL_ROUNDS;
const PARTIAL_ROUNDS: usize = KOALABEAR_POSEIDON2_PARTIAL_ROUNDS_16;
// Permutations per trace row: same work, wider and shorter trace. Used to
// measure how proof size scales with trace width.
#[cfg(feature = "vec2")]
const VECTOR_LEN: usize = 2;
#[cfg(feature = "vec4")]
const VECTOR_LEN: usize = 4;
#[cfg(not(any(feature = "vec2", feature = "vec4")))]
const VECTOR_LEN: usize = 1;

type Val = KoalaBear;
// Challenge field. KoalaBear^4 (124 bits) caps soundness at ~104–108 bits
// whatever the query count, so it cannot reach the 128-bit target.
#[cfg(feature = "ext4")]
type Challenge = BinomialExtensionField<Val, 4>;
#[cfg(feature = "ext5")]
type Challenge = QuinticTrinomialExtensionField<Val>;
#[cfg(not(any(feature = "ext4", feature = "ext5")))]
type Challenge = BinomialExtensionField<Val, 8>;
type ByteHash = Keccak256Hash;
// Commitment digest in u64 words: 4 = 256 bits (128-bit classical collision
// resistance, ~85-bit against BHT quantum collision search); 8 = 512 bits.
#[cfg(feature = "digest512")]
const DIGEST_WORDS: usize = 8;
#[cfg(not(feature = "digest512"))]
const DIGEST_WORDS: usize = 4;
type U64Hash = PaddingFreeSponge<KeccakF, 25, 17, DIGEST_WORDS>;
type FieldHash = SerializingHasher<U64Hash>;
type MyCompress = CompressionFunctionFromHasher<U64Hash, 2, DIGEST_WORDS>;
#[cfg(not(feature = "zk"))]
type ValMmcs = MerkleTreeMmcs<
    [Val; p3_keccak::VECTOR_LEN],
    [u64; p3_keccak::VECTOR_LEN],
    FieldHash,
    MyCompress,
    2,
    DIGEST_WORDS,
>;
// Zero-knowledge mode: salted Merkle leaves + random codewords in FRI. Without
// it a STARK proof leaks information about the witness (the openings are raw
// trace values), which the shielded space cannot accept.
#[cfg(feature = "zk")]
const SALT_ELEMS: usize = 4;
#[cfg(feature = "zk")]
const NUM_RANDOM_CODEWORDS: usize = 8; // ≥ challenge extension degree
#[cfg(feature = "zk")]
type ValMmcs = MerkleTreeHidingMmcs<
    [Val; p3_keccak::VECTOR_LEN],
    [u64; p3_keccak::VECTOR_LEN],
    FieldHash,
    MyCompress,
    StdRng,
    2,
    DIGEST_WORDS,
    SALT_ELEMS,
>;
type ChallengeMmcs = ExtensionMmcs<Val, Challenge, ValMmcs>;
type Challenger = SerializingChallenger32<Val, HashChallenger<u8, ByteHash, 32>>;
type Dft = Radix2DitParallel<Val>;
#[cfg(not(feature = "zk"))]
type Pcs = TwoAdicFriPcs<Val, Dft, ValMmcs, ChallengeMmcs>;
#[cfg(feature = "zk")]
type Pcs = HidingFriPcs<Val, Dft, ValMmcs, ChallengeMmcs, StdRng>;
type MyConfig = StarkConfig<Pcs, Challenge, Challenger>;
type Air = VectorizedPoseidon2Air<
    Val,
    GenericPoseidon2LinearLayersKoalaBear,
    WIDTH,
    SBOX_DEGREE,
    SBOX_REGISTERS,
    HALF_FULL_ROUNDS,
    PARTIAL_ROUNDS,
    VECTOR_LEN,
>;

#[derive(Clone, Copy)]
struct Fri {
    log_blowup: usize,
    max_log_arity: usize,
    query_pow_bits: usize,
}

const SHAPES: &[Fri] = &[
    Fri { log_blowup: 1, max_log_arity: 1, query_pow_bits: 16 },
    Fri { log_blowup: 2, max_log_arity: 1, query_pow_bits: 16 },
    Fri { log_blowup: 3, max_log_arity: 3, query_pow_bits: 16 },
    Fri { log_blowup: 4, max_log_arity: 3, query_pow_bits: 16 },
    Fri { log_blowup: 4, max_log_arity: 3, query_pow_bits: 24 },
    Fri { log_blowup: 5, max_log_arity: 3, query_pow_bits: 16 },
    Fri { log_blowup: 6, max_log_arity: 3, query_pow_bits: 16 },
    Fri { log_blowup: 7, max_log_arity: 3, query_pow_bits: 16 },
    Fri { log_blowup: 8, max_log_arity: 3, query_pow_bits: 16 },
    Fri { log_blowup: 8, max_log_arity: 4, query_pow_bits: 20 },
];

// Bit size of the challenge field. Keccak-256 commitments: 128-bit collision resistance.
#[cfg(feature = "ext4")]
const NUM_MODULUS_BITS: usize = 124;
#[cfg(feature = "ext5")]
const NUM_MODULUS_BITS: usize = 155;
#[cfg(not(any(feature = "ext4", feature = "ext5")))]
const NUM_MODULUS_BITS: usize = 248;
const COLLISION_BITS: usize = DIGEST_WORDS * 64 / 2;
fn target_bits() -> usize {
    std::env::var("TARGET_BITS").ok().and_then(|v| v.parse().ok()).unwrap_or(128)
}
const MAX_QUERIES: usize = 400;

fn regime(f: Fri, num_queries: usize) -> FriRegime {
    FriRegime {
        log_blowup: f.log_blowup,
        num_queries,
        log_final_poly_len: 0,
        max_log_arity: f.max_log_arity,
        commit_pow_bits: 0,
        query_pow_bits: f.query_pow_bits,
    }
}

fn security_params(air: &Air, f: Fri, num_queries: usize, log_n: usize) -> StarkSecurityParams {
    StarkSecurityParams::from_air::<Val, Challenge, _>(
        regime(f, num_queries),
        air,
        AirLayout::from_air(air),
        TwoAdicMultiplicativeCoset::new(Val::ONE, log_n).unwrap(),
        NUM_MODULUS_BITS,
        COLLISION_BITS,
        1, // the Poseidon2 AIR has no transition constraints: one opening point
        #[cfg(not(feature = "zk"))]
        OpeningShape::TwoAdic,
        #[cfg(feature = "zk")]
        OpeningShape::Hiding { num_random_codewords: NUM_RANDOM_CODEWORDS },
        GrindingSites::NONE,
    )
}

fn proven_bits(air: &Air, f: Fri, q: usize, log_n: usize) -> usize {
    ProvenSecurity::compute_from_proof(log_n, &security_params(air, f, q, log_n)).security_bits()
}

fn conj_bits(air: &Air, f: Fri, q: usize, log_n: usize) -> usize {
    ConjecturedSecurity::compute_from_params(&security_params(air, f, q, log_n), log_n).security_bits
}

/// Smallest query count reaching TARGET_BITS, or the best level reachable.
fn min_queries(bits: impl Fn(usize) -> usize) -> (usize, usize) {
    let mut best = (MAX_QUERIES, 0);
    for q in 1..=MAX_QUERIES {
        let b = bits(q);
        if b >= target_bits() {
            return (q, b);
        }
        if b > best.1 {
            best = (q, b);
        }
    }
    best
}

fn run(num_perms: usize, f: Fri, regime_name: &str, num_queries: usize, bits: usize) {
    let u64_hash = U64Hash::new(KeccakF {});
    #[cfg(not(feature = "zk"))]
    let val_mmcs = ValMmcs::new(FieldHash::new(u64_hash), MyCompress::new(u64_hash), 0);
    // Spike only: seeded RNG. A wallet must salt from the OS CSPRNG.
    #[cfg(feature = "zk")]
    let val_mmcs = ValMmcs::new(
        FieldHash::new(u64_hash),
        MyCompress::new(u64_hash),
        0,
        StdRng::seed_from_u64(7),
    );
    let challenge_mmcs = ChallengeMmcs::new(val_mmcs.clone());

    let mut fri_params = FriParameters::new_benchmark(challenge_mmcs);
    fri_params.log_blowup = f.log_blowup;
    fri_params.max_log_arity = f.max_log_arity;
    fri_params.num_queries = num_queries;
    fri_params.query_proof_of_work_bits = f.query_pow_bits;

    let air = make_air();
    #[cfg(not(feature = "zk"))]
    let pcs = Pcs::new(Dft::default(), val_mmcs, fri_params);
    #[cfg(feature = "zk")]
    let pcs = Pcs::new(
        Dft::default(),
        val_mmcs,
        fri_params,
        NUM_RANDOM_CODEWORDS,
        StdRng::seed_from_u64(8),
    );
    let config = MyConfig::new(pcs, Challenger::from_hasher(vec![], ByteHash {}));
    let trace = air.generate_vectorized_trace_rows(num_perms, f.log_blowup);

    let t = Instant::now();
    let proof = prove(&config, &air, trace, &[]).expect("prove");
    let prove_ms = t.elapsed().as_secs_f64() * 1e3;

    let bytes = postcard::to_allocvec(&proof).expect("serialize").len();

    const VERIFY_RUNS: u32 = 20;
    let t = Instant::now();
    for _ in 0..VERIFY_RUNS {
        verify(&config, &air, &proof, &[]).expect("verify");
    }
    let verify_ms = t.elapsed().as_secs_f64() * 1e3 / VERIFY_RUNS as f64;

    println!(
        "| {:>5} | {:>6} | {:>5} | {:>3} | {:<11} | {:>7} | {:>4} | {:>8.1} | {:>8.1} | {:>9.2} |",
        num_perms,
        1usize << f.log_blowup,
        1usize << f.max_log_arity,
        f.query_pow_bits,
        regime_name,
        num_queries,
        bits,
        bytes as f64 / 1024.0,
        prove_ms,
        verify_ms
    );
}

fn make_air() -> Air {
    let mut rng = StdRng::seed_from_u64(1);
    VectorizedPoseidon2Air::new(RoundConstants::from_rng(&mut rng))
}

fn main() {
    println!("trace width = {} columns, height = 128/{} rows", p3_air::BaseAir::<Val>::width(&make_air()), VECTOR_LEN);
    println!("| perms | blowup | arity | pow | regime      | queries | bits | proof KB | prove ms | verify ms |");
    println!("|------:|-------:|------:|----:|-------------|--------:|-----:|---------:|---------:|----------:|");
    let air = make_air();
    for n in env_list("PERMS", &[128, 256]) {
        let log_n = (n / VECTOR_LEN).trailing_zeros() as usize;
        for &f in SHAPES.iter().filter(|f| f.log_blowup >= env_list("MIN_LOG_BLOWUP", &[1])[0]) {
            let (qc, bc) = min_queries(|q| conj_bits(&air, f, q, log_n));
            run(n, f, "conjectured", qc, bc);
            let (qp, bp) = min_queries(|q| proven_bits(&air, f, q, log_n));
            run(n, f, "proven", qp, bp);
        }
    }
}

/// Comma-separated usize list from the environment, e.g. `PERMS=512,1024`.
fn env_list(key: &str, default: &[usize]) -> Vec<usize> {
    std::env::var(key)
        .map(|v| v.split(',').map(|x| x.trim().parse().expect("usize")).collect())
        .unwrap_or_else(|_| default.to_vec())
}
