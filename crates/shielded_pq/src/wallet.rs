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

// ---- recovery from the seed (spec §9.5) -----------------------------------

/// Diversifier index of the wallet's own notes (change, claims).
pub const INTERNAL: u32 = u32::MAX;

/// Consecutive missing tags before a restore stops.
pub const GAP_LIMIT: u32 = 20;

/// Highest self-note index a restore looks at.
pub const MAX_SELF_INDEX: u32 = 1 << 16;

/// The recovery key: derives every self-note and its slot.
pub fn recovery_key(seed: &[u8; 32]) -> [u8; 32] { Xof::new("mazze/rec", &[seed]).bytes() }

/// The public tag of self-note `i`: single-use, unlinkable without the seed.
pub fn recovery_tag(rkey: &[u8; 32], i: u32) -> [u8; 32] {
    Xof::new("mazze/rec/tag", &[rkey, &i.to_le_bytes()]).bytes()
}

impl WalletKeys {
    /// Self-note `i`: everything but the value derives from the seed, so the
    /// wallet can rebuild it from the value its recovery slot holds. Its
    /// hiding rests on SHAKE256 as a PRF (spec §4.2), not on fresh entropy.
    pub fn self_note(&self, sponge: &Sponge, i: u32, value: u64) -> crate::note::Note {
        let mut x = Xof::new("mazze/self-note", &[&recovery_key(&self.seed), &i.to_le_bytes()]);
        let d = self.diversifier(INTERNAL);
        crate::note::Note {
            d,
            owner: self.spend.owner(sponge, &d),
            value,
            psi: x.fields(),
            y: x.fields(),
        }
    }

    /// The recovery slot for self-note `i`: its tag and its encrypted value.
    pub fn seal_recovery(&self, i: u32, value: u64) -> RecoverySlot {
        use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit};
        let rkey = recovery_key(&self.seed);
        let mut plain = [0u8; 64];
        plain[..8].copy_from_slice(&value.to_le_bytes());
        // A single-use key per index, so the fixed nonce is never reused.
        let key: [u8; 32] = Xof::new("mazze/rec/slot", &[&rkey, &i.to_le_bytes()]).bytes();
        let sealed = ChaCha20Poly1305::new(&key.into())
            .encrypt(&[0u8; 12].into(), &plain[..])
            .expect("in-memory encryption");
        RecoverySlot {
            tag: recovery_tag(&rkey, i),
            slot: sealed[..64].try_into().unwrap(),
            mac: sealed[64..].try_into().unwrap(),
        }
    }

    /// The value in self-note `i`'s slot, if `slot` is that slot.
    pub fn open_recovery(&self, i: u32, slot: &RecoverySlot) -> Option<u64> {
        use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit};
        let rkey = recovery_key(&self.seed);
        if slot.tag != recovery_tag(&rkey, i) {
            return None;
        }
        let key: [u8; 32] = Xof::new("mazze/rec/slot", &[&rkey, &i.to_le_bytes()]).bytes();
        let mut sealed = slot.slot.to_vec();
        sealed.extend_from_slice(&slot.mac);
        let plain = ChaCha20Poly1305::new(&key.into())
            .decrypt(&[0u8; 12].into(), &sealed[..])
            .ok()?;
        Some(u64::from_le_bytes(plain[..8].try_into().unwrap()))
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn slots_round_trip_and_stay_private() {
        let sponge = Sponge::new(7);
        let alice = WalletKeys::from_seed(&sponge, [1; 32]);
        let mallory = WalletKeys::from_seed(&sponge, [2; 32]);
        let slot = alice.seal_recovery(3, 123_456);
        assert_eq!(alice.open_recovery(3, &slot), Some(123_456));
        assert_eq!(alice.open_recovery(4, &slot), None, "another index");
        assert_eq!(mallory.open_recovery(3, &slot), None, "another seed");
        let mut tampered = slot;
        tampered.slot[0] ^= 1;
        assert_eq!(alice.open_recovery(3, &tampered), None, "authenticated");
        // Tags are distinct per index and per seed.
        assert_ne!(alice.seal_recovery(4, 1).tag, slot.tag);
        assert_ne!(mallory.seal_recovery(3, 123_456).tag, slot.tag);
    }

    #[test]
    fn self_notes_are_rebuilt_from_the_seed_and_value() {
        let sponge = Sponge::new(7);
        let a = WalletKeys::from_seed(&sponge, [1; 32]);
        let again = WalletKeys::from_seed(&sponge, [1; 32]);
        assert_eq!(a.self_note(&sponge, 5, 99).cm(&sponge), again.self_note(&sponge, 5, 99).cm(&sponge));
        assert_ne!(a.self_note(&sponge, 5, 99).cm(&sponge), a.self_note(&sponge, 6, 99).cm(&sponge));
        assert_ne!(a.self_note(&sponge, 5, 99).cm(&sponge), a.self_note(&sponge, 5, 98).cm(&sponge));
    }
}
