//! Keys and addresses (spec §3). Everything derives from one 32-byte seed.
//!
//! ```text
//! nsk = SHAKE(seed)           the only spend secret
//! ak  = H_AK(nsk), nk = H_NK(nsk)        both derived from nsk (Orchard lesson, Z-1)
//! owner_j = H_OWN(ak ‖ nk ‖ d_j)        nk is committed into every note's owner
//! ```
//!
//! Delivery keys (ML-KEM-768 + Classic McEliece-460896 + X25519) are derived
//! per address from the seed, so the wallet stores none of them and none of
//! them is ever on-chain. Only `H(bundle)` appears, inside the address.

use ml_kem::{EncodedSizeUser, KemCore, MlKem768};
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

use crate::primitives::*;

pub type MlKemDk = <MlKem768 as KemCore>::DecapsulationKey;
pub type MlKemEk = <MlKem768 as KemCore>::EncapsulationKey;

#[derive(Clone)]
pub struct SpendKeys {
    pub nsk: Digest,
    pub ak: Digest,
    pub nk: Digest,
}

impl SpendKeys {
    pub fn from_nsk(nsk: Digest) -> Self {
        let ak = p2_hash(dom::AK, &nsk);
        let nk = p2_hash(dom::NK, &nsk);
        SpendKeys { nsk, ak, nk }
    }

    pub fn owner(&self, d: &[F; 3]) -> Digest {
        let mut input = Vec::with_capacity(2 * DIGEST + 3);
        input.extend_from_slice(&self.ak);
        input.extend_from_slice(&self.nk);
        input.extend_from_slice(d);
        p2_hash(dom::OWN, &input)
    }
}

#[derive(Clone)]
pub struct Seed(pub [u8; 32]);

impl Seed {
    pub fn random() -> Self {
        Seed(rand::random())
    }

    pub fn spend_keys(&self) -> SpendKeys {
        SpendKeys::from_nsk(Xof::new("mazze/nsk", &[&self.0]).fields())
    }

    pub fn diversifier(&self, j: u32) -> [F; 3] {
        Xof::new("mazze/div", &[&self.0, &j.to_le_bytes()]).fields()
    }

    /// Key for the recovery slots (symmetric only).
    pub fn rkey(&self) -> [u8; 32] {
        Xof::new("mazze/rec", &[&self.0]).bytes()
    }

    /// Key for recipient-blind outgoing payments: it derives the note
    /// randomness from the seed and an index, using no recipient data.
    pub fn out_key(&self) -> [u8; 32] {
        Xof::new("mazze/out", &[&self.0]).bytes()
    }

    pub fn delivery_keys(&self, j: u32) -> DeliveryKeys {
        let mut x = Xof::new("mazze/delivery", &[&self.0, &j.to_le_bytes()]);
        let (dz_d, dz_z): ([u8; 32], [u8; 32]) = (x.bytes(), x.bytes());
        let (mlkem_dk, mlkem_ek) =
            MlKem768::generate_deterministic(&dz_d.into(), &dz_z.into());
        let mut mce_rng = ChaCha20Rng::from_seed(x.bytes());
        let (mce_pk, mce_sk) = classic_mceliece_rust::keypair_boxed(&mut mce_rng);
        let x_sk = x25519_dalek::StaticSecret::from(x.bytes::<32>());
        let x_pk = x25519_dalek::PublicKey::from(&x_sk);
        DeliveryKeys { mlkem_dk, mlkem_ek, mce_pk, mce_sk, x_sk, x_pk }
    }

    /// Address `j`: what the recipient publishes.
    pub fn address(&self, j: u32) -> (Address, Bundle) {
        let keys = self.spend_keys();
        let d = self.diversifier(j);
        let owner = keys.owner(&d);
        let bundle = self.delivery_keys(j).bundle();
        let addr = Address { d, owner, bundle_hash: bundle.hash() };
        (addr, bundle)
    }
}

pub struct DeliveryKeys {
    pub mlkem_dk: MlKemDk,
    pub mlkem_ek: MlKemEk,
    pub mce_pk: classic_mceliece_rust::PublicKey<'static>,
    pub mce_sk: classic_mceliece_rust::SecretKey<'static>,
    pub x_sk: x25519_dalek::StaticSecret,
    pub x_pk: x25519_dalek::PublicKey,
}

impl DeliveryKeys {
    pub fn bundle(&self) -> Bundle {
        let mut bytes = vec![1u8]; // bundle version
        bytes.extend_from_slice(self.mlkem_ek.as_bytes().as_slice());
        bytes.extend_from_slice(self.mce_pk.as_ref());
        bytes.extend_from_slice(self.x_pk.as_bytes());
        Bundle(bytes)
    }
}

pub const MLKEM_EK_LEN: usize = 1184;
pub const MCE_PK_LEN: usize = classic_mceliece_rust::CRYPTO_PUBLICKEYBYTES;

/// The recipient's public delivery keys. They travel off-chain, once per
/// relationship, and are verified against `Address::bundle_hash`.
#[derive(Clone)]
pub struct Bundle(pub Vec<u8>);

impl Bundle {
    pub fn hash(&self) -> [u8; 32] {
        Xof::new("mazze/bundle-hash", &[&self.0]).bytes()
    }
}

/// The address itself, which is enough to *create* a payment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    pub d: [F; 3],
    pub owner: Digest,
    pub bundle_hash: [u8; 32],
}

impl Address {
    pub fn encode(&self) -> String {
        let mut b = vec![1u8];
        b.extend(to_bytes(&self.d));
        b.extend(to_bytes(&self.owner));
        b.extend_from_slice(&self.bundle_hash);
        format!("mzs1{}", base32(&b))
    }

    pub fn decode(s: &str) -> Option<Self> {
        let b = unbase32(s.strip_prefix("mzs1")?)?;
        if b.len() != 1 + 12 + 48 + 32 || b[0] != 1 {
            return None;
        }
        Some(Address {
            d: from_bytes(&b[1..13])?,
            owner: from_bytes(&b[13..61])?,
            bundle_hash: b[61..93].try_into().ok()?,
        })
    }
}
