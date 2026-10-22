//! Proof system PS-1 (spec §6.4): KoalaBear with a degree-8 extension, FRI,
//! 512-bit Keccak commitments, Keccak-256 Fiat–Shamir, hiding FRI
//! (zero-knowledge), 192 bits of proven soundness: about 96 against a
//! quantum adversary, whose QROM advantage is roughly quadratic.
//!
//! Every parameter is a constant. The query count was chosen with the
//! `p3-security` estimator once, and a test re-checks it; the estimator is
//! never consulted at run time, so updating it cannot change consensus.

use p3_challenger::{HashChallenger, SerializingChallenger32};
use p3_commit::ExtensionMmcs;
use p3_dft::Radix2DitParallel;
use p3_field::extension::BinomialExtensionField;
use p3_fri::{FriParameters, HidingFriPcs};
use p3_keccak::{Keccak256Hash, KeccakF};
use p3_merkle_tree::MerkleTreeHidingMmcs;
use p3_symmetric::{
    CompressionFunctionFromHasher, PaddingFreeSponge, SerializingHasher,
};
use p3_air::{Air, BaseAir};
use p3_uni_stark::{
    QuotientAir, StarkConfig, SymbolicAirBuilder, VerifierConstraintFolder,
};
use rand::{rngs::StdRng, SeedableRng};

use crate::field::F;

pub const PROOF_SYSTEM_ID: u8 = 1;

pub const LOG_BLOWUP: usize = 6;
pub const MAX_LOG_ARITY: usize = 3;
/// The fewest queries that reach 192 proven bits at this blowup (59 gives
/// 192; blowup 2^7 needs 51 but doubles the proving time for a ~6% smaller
/// proof, measured 2026-10-22).
pub const NUM_QUERIES: usize = 59;
pub const QUERY_POW_BITS: usize = 16;
pub const CAP_HEIGHT: usize = 0;
/// Salt per Merkle leaf of the hiding commitments: 8 elements (~248 bits,
/// ~124 against Grover), so the openings hide the trace against a quantum
/// adversary with the same margin as the soundness target.
pub const SALT_ELEMS: usize = 8;
pub const NUM_RANDOM_CODEWORDS: usize = 8;

/// Proven soundness the constants above give for the 2×2 AIR.
pub const PROVEN_SECURITY_BITS: usize = 192;

/// The size of every proof, in bytes (spec §6.2): encodings measure
/// 388–400 KB (proving ~1.6 s, verifying ~7 ms) and are padded with zeros
/// to exactly this, so proof size tells nothing; a prover whose encoding
/// exceeds it re-proves.
pub const PROOF_LEN_MAX: usize = 400 * 1024;

// Blocks bound proofs by the same cap before any proof system is consulted.
const _: () = assert!(PROOF_LEN_MAX == primitives::shielded::MAX_PROOF_BYTES);

pub type Challenge = BinomialExtensionField<F, 8>;
type ByteHash = Keccak256Hash;
/// Merkle digests of 8 Keccak lanes, 512 bits: with 256 bits the commitment's
/// collision resistance caps proven soundness at 128 bits whatever the query
/// count, and a quantum collision search (BHT, ~2^(n/3)) would cut that to
/// ~85. At 512 bits the term is 256 classical, ~170 quantum.
pub const DIGEST_WORDS: usize = 8;
type U64Hash = PaddingFreeSponge<KeccakF, 25, 17, DIGEST_WORDS>;
type FieldHash = SerializingHasher<U64Hash>;
type Compress = CompressionFunctionFromHasher<U64Hash, 2, DIGEST_WORDS>;
type ValMmcs = MerkleTreeHidingMmcs<
    [F; p3_keccak::VECTOR_LEN],
    [u64; p3_keccak::VECTOR_LEN],
    FieldHash,
    Compress,
    StdRng,
    2,
    DIGEST_WORDS,
    SALT_ELEMS,
>;
type ChallengeMmcs = ExtensionMmcs<F, Challenge, ValMmcs>;
type Challenger = SerializingChallenger32<F, HashChallenger<u8, ByteHash, 32>>;
type Dft = Radix2DitParallel<F>;
type Pcs = HidingFriPcs<F, Dft, ValMmcs, ChallengeMmcs, StdRng>;
pub type Config = StarkConfig<Pcs, Challenge, Challenger>;
pub type Proof = p3_uni_stark::Proof<Config>;

/// Fiat–Shamir domain (spec §6.3): a proof is valid only for one chain, one
/// proof system and one circuit. The challenger starts from these bytes, so
/// every challenge depends on them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Domain {
    pub chain_id: u32,
    pub air_id: &'static str,
}

impl Domain {
    pub fn transcript_seed(&self) -> Vec<u8> {
        let mut v = b"mazze/shielded/fiat-shamir/v1".to_vec();
        v.push(PROOF_SYSTEM_ID);
        v.extend_from_slice(&self.chain_id.to_le_bytes());
        v.extend_from_slice(&(self.air_id.len() as u32).to_le_bytes());
        v.extend_from_slice(self.air_id.as_bytes());
        v
    }
}

/// Fresh randomness for one proof: the Merkle salts and the random codewords
/// that make the proof zero-knowledge. Reusing or deriving it predictably
/// breaks zero-knowledge, so it is taken only from the caller's CSPRNG and
/// consumed by value.
pub struct ProverRandomness {
    salts: [u8; 32],
    codewords: [u8; 32],
}

impl ProverRandomness {
    pub fn from_csprng<R: rand::CryptoRng + ?Sized>(rng: &mut R) -> Self {
        let mut salts = [0u8; 32];
        let mut codewords = [0u8; 32];
        rng.fill_bytes(&mut salts);
        rng.fill_bytes(&mut codewords);
        ProverRandomness { salts, codewords }
    }
}

fn build(domain: Domain, salts: StdRng, codewords: StdRng) -> Config {
    let u64_hash = U64Hash::new(KeccakF {});
    let val_mmcs = ValMmcs::new(
        FieldHash::new(u64_hash),
        Compress::new(u64_hash),
        CAP_HEIGHT,
        salts,
    );
    let challenge_mmcs = ChallengeMmcs::new(val_mmcs.clone());
    let mut fri = FriParameters::new_benchmark(challenge_mmcs);
    fri.log_blowup = LOG_BLOWUP;
    fri.max_log_arity = MAX_LOG_ARITY;
    fri.num_queries = NUM_QUERIES;
    fri.query_proof_of_work_bits = QUERY_POW_BITS;
    let pcs =
        Pcs::new(Dft::default(), val_mmcs, fri, NUM_RANDOM_CODEWORDS, codewords);
    Config::new(
        pcs,
        Challenger::from_hasher(domain.transcript_seed(), ByteHash {}),
    )
}

/// The configuration a verifier uses. Verification draws no randomness; the
/// generators exist only because the prover and verifier share one type.
pub fn verifier_config(domain: Domain) -> Config {
    build(domain, StdRng::from_seed([0; 32]), StdRng::from_seed([0; 32]))
}

/// The configuration for one proof.
pub fn prover_config(domain: Domain, randomness: ProverRandomness) -> Config {
    build(
        domain,
        StdRng::from_seed(randomness.salts),
        StdRng::from_seed(randomness.codewords),
    )
}

/// What the prover and verifier need from an AIR.
pub trait Ps1Air:
    BaseAir<F>
    + Air<SymbolicAirBuilder<F, Challenge>>
    + Air<SymbolicAirBuilder<F>>
    + for<'a> Air<VerifierConstraintFolder<'a, Config>>
    + QuotientAir<Config>
{
    /// Circuit identifier for Fiat–Shamir domain separation (name + version).
    fn air_id(&self) -> &'static str;
}

fn domain_of<A: Ps1Air>(air: &A, chain_id: u32) -> Domain {
    Domain {
        chain_id,
        air_id: air.air_id(),
    }
}

/// Verifies `proof` for `air` on chain `chain_id` against `public` values.
pub fn verify<A: Ps1Air>(
    air: &A, chain_id: u32, proof: &Proof, public: &[F],
) -> Result<(), String> {
    let config = verifier_config(domain_of(air, chain_id));
    p3_uni_stark::verify(&config, air, proof, public)
        .map_err(|e| format!("{e:?}"))
}

/// Proves `trace` for `air` on chain `chain_id`, with fresh randomness.
#[cfg(feature = "prover")]
pub fn prove<A: Ps1Air>(
    air: &A, chain_id: u32, trace: p3_matrix::dense::RowMajorMatrix<F>,
    public: &[F], randomness: ProverRandomness,
) -> Result<Proof, String>
where
    A: for<'a> Air<p3_air::DebugConstraintBuilder<'a, F>>,
{
    let config = prover_config(domain_of(air, chain_id), randomness);
    p3_uni_stark::prove(&config, air, trace, public)
        .map_err(|e| format!("{e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_separate_chain_and_circuit() {
        let a = Domain { chain_id: 1, air_id: "mazze/shielded/spend-2x2/v1" };
        let b = Domain { chain_id: 2, ..a };
        let c = Domain { air_id: "mazze/shielded/spend-2x2/v2", ..a };
        assert_ne!(a.transcript_seed(), b.transcript_seed());
        assert_ne!(a.transcript_seed(), c.transcript_seed());
        // The circuit id is length-prefixed, so no id is a prefix of another's
        // encoding.
        let d = Domain { air_id: "mazze/shielded/spend-2x2/v", ..a };
        assert!(!a.transcript_seed().starts_with(&d.transcript_seed()));
    }
}
