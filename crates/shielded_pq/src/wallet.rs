//! Building a shielded transaction (wallet side): fix the body, bind it with
//! the sighash, prove, then commit the body to its proof.

use p3_field::PrimeField32;
use primitives::shielded::{
    proof_hash, RecoverySlot, ShieldedDest, ShieldedDigest, ShieldedTxBody,
};

use crate::{
    air::spend2x2::{build_trace, pvi, Tx2Air, Tx2Witness, HEIGHT},
    field::{Digest, Sponge, Xof, DIGEST, F},
    keys::SpendKeys,
    note::HM_L,
    ps1::{self, ProverRandomness, PROOF_LEN_MAX},
    wire,
};

pub fn to_shielded_digest(xs: &[F]) -> ShieldedDigest {
    ShieldedDigest::from_elements(std::array::from_fn(|i| xs[i].as_canonical_u32()))
        .expect("field elements are canonical")
}

pub fn from_shielded_digest(d: &ShieldedDigest) -> Digest {
    crate::field::from_bytes::<DIGEST>(d.as_bytes()).expect("canonical digest")
}

/// Spend keys and diversifiers derived from a 32-byte seed (spec §3).
pub struct WalletKeys {
    pub seed: [u8; 32],
    pub spend: SpendKeys,
}

impl WalletKeys {
    pub fn from_seed(sponge: &Sponge, seed: [u8; 32]) -> Self {
        let nsk = Xof::new("mazze/nsk", &[&seed]).fields();
        WalletKeys { seed, spend: SpendKeys::from_nsk(sponge, nsk) }
    }

    pub fn diversifier(&self, j: u32) -> [F; 3] {
        Xof::new("mazze/div", &[&self.seed, &j.to_le_bytes()]).fields()
    }

    pub fn owner(&self, sponge: &Sponge, j: u32) -> Digest {
        self.spend.owner(sponge, &self.diversifier(j))
    }
}

/// Fresh uniform field elements from OS randomness (the no-trace mode: the
/// commitment is statistically hiding, spec §4.2).
pub fn fresh_fields<const N: usize>(entropy: &[u8; 32]) -> [F; N] {
    Xof::new("mazze/os-random", &[entropy]).fields()
}

pub fn fresh_y(entropy: &[u8; 32]) -> [F; HM_L] { fresh_fields(entropy) }

/// Proves `w` and returns the body and its proof. `w.sighash` is set here.
/// A proof longer than the cap is redone with new randomness.
pub fn build_transaction(
    chain_id: u32, w: &mut Tx2Witness, dest: Option<ShieldedDest>,
    recovery: RecoverySlot, mut randomness: impl FnMut() -> ProverRandomness,
) -> Result<(ShieldedTxBody, Vec<u8>), String> {
    let (_, pv) = build_trace(w, chain_id, HEIGHT, None);
    let at = |o: usize| to_shielded_digest(&pv[o..o + DIGEST]);
    let mut body = ShieldedTxBody {
        pool_version: crate::verify::POOL_VERSION,
        proof_system_id: ps1::PROOF_SYSTEM_ID,
        anchor: at(pvi::ANCHOR),
        nf: [at(pvi::NF[0]), at(pvi::NF[1])],
        cm_out: [at(pvi::CM[0]), at(pvi::CM[1])],
        recovery,
        v_public_in: w.v_in,
        v_public_out: w.v_out,
        dest,
        fee: w.fee,
        proof_hash: Default::default(),
    };
    body.check_structure().map_err(|e| e.to_string())?;
    w.sighash = body.sighash(chain_id);
    let (trace, pv) = build_trace(w, chain_id, HEIGHT, None);
    let air = Tx2Air::new(chain_id);
    loop {
        let proof = ps1::prove(&air, chain_id, trace.clone(), &pv, randomness())?;
        let bytes = wire::encode(&proof);
        if bytes.len() <= PROOF_LEN_MAX {
            body.proof_hash = proof_hash(&bytes);
            return Ok((body, bytes));
        }
    }
}
