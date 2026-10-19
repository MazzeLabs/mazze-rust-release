//! ZK STARK configuration (KoalaBear^8, Keccak commitments, hiding FRI) and
//! prove/verify for the M1 AIR, with query counts chosen by `p3-security` for a
//! *proven* soundness target.

use std::time::Instant;

use p3_challenger::{HashChallenger, SerializingChallenger32};
use p3_commit::ExtensionMmcs;
use p3_dft::Radix2DitParallel;
use p3_field::coset::TwoAdicMultiplicativeCoset;
use p3_field::extension::BinomialExtensionField;
use p3_field::PrimeCharacteristicRing;
use p3_fri::{FriParameters, HidingFriPcs};
use p3_keccak::{Keccak256Hash, KeccakF};
use p3_matrix::dense::RowMajorMatrix;
use p3_merkle_tree::MerkleTreeHidingMmcs;
use p3_security::fri::FriRegime;
use p3_symmetric::{CompressionFunctionFromHasher, PaddingFreeSponge, SerializingHasher};
use p3_air::{Air, BaseAir, DebugConstraintBuilder};
use p3_uni_stark::{
    prove, verify, AirLayout, GrindingSites, OpeningShape, ProvenSecurity, QuotientAir, StarkConfig,
    StarkSecurityParams, SymbolicAirBuilder, VerifierConstraintFolder,
};
use rand::rngs::StdRng;
use rand::SeedableRng;

use shielded_notes_spike::primitives::F;


type Challenge = BinomialExtensionField<F, 8>;
type ByteHash = Keccak256Hash;
type U64Hash = PaddingFreeSponge<KeccakF, 25, 17, 4>;
type FieldHash = SerializingHasher<U64Hash>;
type MyCompress = CompressionFunctionFromHasher<U64Hash, 2, 4>;
const SALT_ELEMS: usize = 4;
const NUM_RANDOM_CODEWORDS: usize = 8;
type ValMmcs = MerkleTreeHidingMmcs<
    [F; p3_keccak::VECTOR_LEN],
    [u64; p3_keccak::VECTOR_LEN],
    FieldHash,
    MyCompress,
    StdRng,
    2,
    4,
    SALT_ELEMS,
>;
type ChallengeMmcs = ExtensionMmcs<F, Challenge, ValMmcs>;
type Challenger = SerializingChallenger32<F, HashChallenger<u8, ByteHash, 32>>;
type Dft = Radix2DitParallel<F>;
type Pcs = HidingFriPcs<F, Dft, ValMmcs, ChallengeMmcs, StdRng>;
pub type Config = StarkConfig<Pcs, Challenge, Challenger>;

pub const NUM_MODULUS_BITS: usize = 248;
pub const COLLISION_BITS: usize = 128;

#[derive(Clone, Copy, Debug)]
pub struct FriShape {
    /// Merkle cap height: commitments publish 2^cap nodes, every path is `cap` levels shorter.
    pub cap_height: usize,
    pub log_blowup: usize,
    pub max_log_arity: usize,
    pub query_pow_bits: usize,
}

fn regime(f: FriShape, q: usize) -> FriRegime {
    FriRegime {
        log_blowup: f.log_blowup,
        num_queries: q,
        log_final_poly_len: 0,
        max_log_arity: f.max_log_arity,
        commit_pow_bits: 0,
        query_pow_bits: f.query_pow_bits,
    }
}

/// Fiat–Shamir domain (spec §6.4): a proof is valid only for this chain, this
/// proof system and this circuit. The challenger starts from these bytes, so
/// every challenge depends on them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Domain {
    pub proof_system_id: u8,
    pub chain_id: u32,
    pub air_id: &'static str,
}

pub const PS_1: u8 = 1;

impl Domain {
    pub fn of<A: ProvableAir>(air: &A) -> Self {
        Domain { proof_system_id: PS_1, chain_id: shielded_notes_spike::primitives::CHAIN_ID, air_id: air.air_id() }
    }
    pub fn transcript_seed(&self) -> Vec<u8> {
        let mut v = b"mazze/shielded/fiat-shamir/v1".to_vec();
        v.push(self.proof_system_id);
        v.extend_from_slice(&self.chain_id.to_le_bytes());
        v.extend_from_slice(&(self.air_id.len() as u32).to_le_bytes());
        v.extend_from_slice(self.air_id.as_bytes());
        v
    }
}

/// Everything the prover, verifier and security estimator need from an AIR.
pub trait ProvableAir:
    BaseAir<F>
    + Air<SymbolicAirBuilder<F, Challenge>>
    + Air<SymbolicAirBuilder<F>>
    + for<'a> Air<VerifierConstraintFolder<'a, Config>>
    + for<'a> Air<DebugConstraintBuilder<'a, F>>
    + QuotientAir<Config>
{
    fn trace_height(&self) -> usize;
    /// Circuit identifier for Fiat–Shamir domain separation (name + version).
    fn air_id(&self) -> &'static str;
}

/// Smallest query count giving `target` proven bits for this AIR at this height.
pub fn proven_queries<A: ProvableAir>(air: &A, f: FriShape, target: usize) -> (usize, usize) {
    let log_n = air.trace_height().trailing_zeros() as usize;
    let mut best = (0, 0);
    for q in 1..=400 {
        let params = StarkSecurityParams::from_air::<F, Challenge, _>(
            regime(f, q),
            air,
            AirLayout::from_air(air),
            TwoAdicMultiplicativeCoset::new(F::ONE, log_n).unwrap(),
            NUM_MODULUS_BITS,
            COLLISION_BITS,
            2, // the AIR reads the next row
            OpeningShape::Hiding { num_random_codewords: NUM_RANDOM_CODEWORDS },
            GrindingSites::NONE,
        );
        let bits = ProvenSecurity::compute_from_proof(log_n, &params).security_bits();
        if bits >= target {
            return (q, bits);
        }
        if bits > best.1 {
            best = (q, bits);
        }
    }
    best
}

pub fn config(f: FriShape, num_queries: usize, seed: u64, domain: Domain) -> Config {
    let u64_hash = U64Hash::new(KeccakF {});
    // Spike only: seeded RNG for salts/codewords. A wallet uses the OS CSPRNG.
    let val_mmcs =
        ValMmcs::new(FieldHash::new(u64_hash), MyCompress::new(u64_hash), f.cap_height, StdRng::seed_from_u64(seed));
    let challenge_mmcs = ChallengeMmcs::new(val_mmcs.clone());
    let mut fri = FriParameters::new_benchmark(challenge_mmcs);
    fri.log_blowup = f.log_blowup;
    fri.max_log_arity = f.max_log_arity;
    fri.num_queries = num_queries;
    fri.query_proof_of_work_bits = f.query_pow_bits;
    let pcs = Pcs::new(Dft::default(), val_mmcs, fri, NUM_RANDOM_CODEWORDS, StdRng::seed_from_u64(seed + 1));
    Config::new(pcs, Challenger::from_hasher(domain.transcript_seed(), ByteHash {}))
}

pub struct Measured {
    /// Bytes per component: (out-of-domain values, trace/quotient/random query openings, FRI folding layers, final poly + commitments)
    pub breakdown: [usize; 4],
    pub queries: usize,
    pub bits: usize,
    pub proof_bytes: usize,
    pub prove_ms: f64,
    pub verify_ms: f64,
}

pub fn prove_and_verify<A: ProvableAir>(
    air: &A,
    trace: RowMajorMatrix<F>,
    public: &[F],
    f: FriShape,
    target_bits: usize,
) -> Result<Measured, String> {
    let (queries, bits) = proven_queries(air, f, target_bits);
    let cfg = config(f, queries, 7, Domain::of(air));
    let t = Instant::now();
    let proof = prove(&cfg, air, trace, public).map_err(|e| format!("prove: {e:?}"))?;
    let prove_ms = t.elapsed().as_secs_f64() * 1e3;
    let proof_bytes = postcard::to_allocvec(&proof).unwrap().len();
    let sz = |x: &dyn erased::Ser| x.len();
    // Hiding FRI: (opened values at the OOD point for the hiding batch, FRI proof).
    let (hiding_values, fri_proof) = &proof.opening_proof;
    let ood = sz(&proof.opened_values) + sz(hiding_values);
    let inputs = sz(&fri_proof.input_openings);
    let fri = sz(&fri_proof.commit_phase_openings);
    let breakdown = [ood, inputs, fri, proof_bytes - ood - inputs - fri];
    if std::env::var("BREAKDOWN").is_ok() {
        // One multi-opening per commitment round (trace, quotient, random…), shared by all queries.
        for (i, b) in fri_proof.input_openings.iter().enumerate() {
            let width: usize = b.opened_values.first().map(|q| q.iter().map(|v| v.len()).sum()).unwrap_or(0);
            eprintln!(
                "  round {i}: opened values {:.1} KB (width {width} per query), Merkle multiproof {:.1} KB",
                sz(&b.opened_values) as f64 / 1024.0,
                sz(&b.opening_proof) as f64 / 1024.0
            );
        }
        for (i, st) in fri_proof.commit_phase_openings.iter().enumerate() {
            eprintln!(
                "  FRI layer {i}: siblings {:.1} KB, multiproof {:.1} KB",
                sz(&st.sibling_values) as f64 / 1024.0,
                sz(&st.opening_proof) as f64 / 1024.0
            );
        }
    }
    let t = Instant::now();
    verify(&cfg, air, &proof, public).map_err(|e| format!("verify: {e:?}"))?;
    let verify_ms = t.elapsed().as_secs_f64() * 1e3;
    Ok(Measured { breakdown, queries, bits, proof_bytes, prove_ms, verify_ms })
}

/// Proves and returns the proof object with its config, for wire-format tests.
pub fn prove_proof<A: ProvableAir>(
    air: &A,
    trace: RowMajorMatrix<F>,
    public: &[F],
    f: FriShape,
    target_bits: usize,
) -> Result<(Config, p3_uni_stark::Proof<Config>), String> {
    let (queries, _) = proven_queries(air, f, target_bits);
    let cfg = config(f, queries, 7, Domain::of(air));
    let proof = prove(&cfg, air, trace, public).map_err(|e| format!("prove: {e:?}"))?;
    Ok((cfg, proof))
}

pub fn verify_proof<A: ProvableAir>(
    cfg: &Config,
    air: &A,
    proof: &p3_uni_stark::Proof<Config>,
    public: &[F],
) -> Result<(), String> {
    verify(cfg, air, proof, public).map_err(|e| format!("verify: {e:?}"))
}

/// Prove with the given public values, then verify against `claimed` public
/// values. It must fail whenever `claimed != public`.
pub fn verify_with_other_public<A: ProvableAir>(
    air: &A,
    trace: RowMajorMatrix<F>,
    public: &[F],
    claimed: &[F],
    f: FriShape,
) -> bool {
    let cfg = config(f, 8, 9, Domain::of(air));
    let proof = prove(&cfg, air, trace, public).expect("honest proof");
    verify(&cfg, air, &proof, claimed).is_ok()
}

/// Prove under `prove_domain`, verify under `verify_domain`. It must fail
/// whenever the domains differ.
pub fn verify_across_domains<A: ProvableAir>(
    air: &A,
    trace: RowMajorMatrix<F>,
    public: &[F],
    f: FriShape,
    prove_domain: Domain,
    verify_domain: Domain,
) -> bool {
    let proof = prove(&config(f, 8, 9, prove_domain), air, trace, public).expect("honest proof");
    verify(&config(f, 8, 9, verify_domain), air, &proof, public).is_ok()
}

mod erased {
    /// Serialized length of any proof component.
    pub trait Ser {
        fn len(&self) -> usize;
    }
    impl<T: serde::Serialize> Ser for T {
        fn len(&self) -> usize {
            postcard::to_allocvec(self).map(|v| v.len()).unwrap_or(0)
        }
    }
}

