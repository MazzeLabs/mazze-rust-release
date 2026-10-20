//! Addresses and note delivery (spec §3, §9.2–9.3).
//!
//! - An address `mzs1…` is `d ‖ owner ‖ H(bundle)`: enough to create a
//!   payment. The bundle holds the recipient's three public delivery keys and
//!   travels off-chain, once per relationship; it is checked against the
//!   hash in the address. Nothing about delivery is on chain.
//! - A plain opening `mzn1…` is for channels that are already private (in
//!   person, a file, paper).
//! - A sealed opening `mzx1…` is the opening under a hybrid KEM: ML-KEM-768
//!   (lattices) + Classic McEliece-460896 (Goppa codes) + X25519. The KDF
//!   hashes all three secrets and all ciphertexts, so the blob stays secret
//!   while any one component holds. It carries no sender signature or
//!   identifier (anyone holding the bundle could have produced it) and is
//!   key-committing.
//!
//! The derivation labels are those of the prototype (spikes/shielded-notes),
//! so both produce the same addresses and bundles from a seed.

use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit};
use ml_kem::{
    kem::{Decapsulate, Encapsulate},
    EncodedSizeUser, KemCore, MlKem768,
};
use mazze_shielded_pq::{
    field::{from_bytes, to_bytes, Digest, Sponge, Xof, DIGEST, F},
    note::HM_L,
    wallet::WalletKeys,
};
use rand::{rngs::OsRng, SeedableRng};

type MlKemDk = <MlKem768 as KemCore>::DecapsulationKey;
type MlKemEk = <MlKem768 as KemCore>::EncapsulationKey;

pub const MLKEM_EK_LEN: usize = 1184;
pub const MLKEM_CT_LEN: usize = 1088;
pub const MCE_PK_LEN: usize = classic_mceliece_rust::CRYPTO_PUBLICKEYBYTES;
pub const MCE_CT_LEN: usize = classic_mceliece_rust::CRYPTO_CIPHERTEXTBYTES;
pub const OPENING_LEN: usize = 1 + 12 + 8 + 4 * DIGEST + 4 * HM_L;
pub const SEALED_LEN: usize = 1 + MLKEM_CT_LEN + MCE_CT_LEN + 32 + 32 + OPENING_LEN + 16;

// ---- base32 (RFC 4648, lower case, no padding) --------------------------

pub fn base32(data: &[u8]) -> String {
    const A: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in data {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(A[((buf >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(A[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

pub fn unbase32(s: &str) -> Option<Vec<u8>> {
    let (mut buf, mut bits, mut out) = (0u32, 0u32, Vec::new());
    for c in s.trim().bytes() {
        let v = match c {
            b'a'..=b'z' => c - b'a',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        } as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

// ---- addresses and bundles ---------------------------------------------------

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
        let b = unbase32(s.trim().strip_prefix("mzs1")?)?;
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

/// The recipient's three public delivery keys, versioned.
pub struct Bundle(pub Vec<u8>);

impl Bundle {
    pub fn hash(&self) -> [u8; 32] { Xof::new("mazze/bundle-hash", &[&self.0]).bytes() }
}

pub struct DeliveryKeys {
    mlkem_dk: MlKemDk,
    mlkem_ek: MlKemEk,
    mce_pk: classic_mceliece_rust::PublicKey<'static>,
    mce_sk: classic_mceliece_rust::SecretKey<'static>,
    x_sk: x25519_dalek::StaticSecret,
    x_pk: x25519_dalek::PublicKey,
}

impl DeliveryKeys {
    /// The delivery keys of address `j`, derived from the seed, so the
    /// wallet stores none of them.
    pub fn derive(seed: &[u8; 32], j: u32) -> Self {
        let mut x = Xof::new("mazze/delivery", &[seed, &j.to_le_bytes()]);
        let (dz_d, dz_z): ([u8; 32], [u8; 32]) = (x.bytes(), x.bytes());
        let (mlkem_dk, mlkem_ek) = MlKem768::generate_deterministic(&dz_d.into(), &dz_z.into());
        let mut mce_rng = rand_chacha::ChaCha20Rng::from_seed(x.bytes());
        let (mce_pk, mce_sk) = classic_mceliece_rust::keypair_boxed(&mut mce_rng);
        let x_sk = x25519_dalek::StaticSecret::from(x.bytes::<32>());
        let x_pk = x25519_dalek::PublicKey::from(&x_sk);
        DeliveryKeys { mlkem_dk, mlkem_ek, mce_pk, mce_sk, x_sk, x_pk }
    }

    pub fn bundle(&self) -> Bundle {
        let mut bytes = vec![1u8];
        bytes.extend_from_slice(self.mlkem_ek.as_bytes().as_slice());
        bytes.extend_from_slice(self.mce_pk.as_ref());
        bytes.extend_from_slice(self.x_pk.as_bytes());
        Bundle(bytes)
    }
}

/// Address `j` of a wallet, with its bundle.
pub fn address(keys: &WalletKeys, sponge: &Sponge, j: u32) -> (Address, Bundle) {
    let d = keys.diversifier(j);
    let bundle = DeliveryKeys::derive(&keys.seed, j).bundle();
    let addr = Address { d, owner: keys.spend.owner(sponge, &d), bundle_hash: bundle.hash() };
    (addr, bundle)
}

// ---- openings ------------------------------------------------------------------

/// What the recipient needs to spend a note. The owner is not in it: the
/// recipient recomputes it from its own keys and `d`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opening {
    pub d: [F; 3],
    pub value: u64,
    pub psi: Digest,
    pub y: [F; HM_L],
}

impl Opening {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = vec![1u8];
        v.extend(to_bytes(&self.d));
        v.extend(self.value.to_le_bytes());
        v.extend(to_bytes(&self.psi));
        v.extend(to_bytes(&self.y));
        debug_assert_eq!(v.len(), OPENING_LEN);
        v
    }

    /// Canonical decoding only: exact length, known version, field elements
    /// below the prime.
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() != OPENING_LEN || b[0] != 1 {
            return None;
        }
        Some(Opening {
            d: from_bytes(&b[1..13])?,
            value: u64::from_le_bytes(b[13..21].try_into().ok()?),
            psi: from_bytes(&b[21..69])?,
            y: from_bytes(&b[69..])?,
        })
    }

    pub fn to_plain(&self) -> String { format!("mzn1{}", base32(&self.to_bytes())) }

    pub fn from_plain(s: &str) -> Option<Self> { Self::from_bytes(&unbase32(s.trim().strip_prefix("mzn1")?)?) }
}

// ---- sealing ---------------------------------------------------------------------

fn kdf(ss: [&[u8]; 3], cts: [&[u8]; 3], bundle_hash: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let k: [u8; 32] =
        Xof::new("mazze/seal/kdf/v1", &[ss[0], ss[1], ss[2], cts[0], cts[1], cts[2], bundle_hash]).bytes();
    let commit: [u8; 32] = Xof::new("mazze/seal/key-commit/v1", &[&k]).bytes();
    (k, commit)
}

fn parse_bundle(
    b: &Bundle,
) -> Option<(MlKemEk, classic_mceliece_rust::PublicKey<'static>, x25519_dalek::PublicKey)> {
    let raw = &b.0;
    if raw.len() != 1 + MLKEM_EK_LEN + MCE_PK_LEN + 32 || raw[0] != 1 {
        return None;
    }
    let ek = MlKemEk::from_bytes(raw[1..1 + MLKEM_EK_LEN].try_into().ok()?);
    let mce: Box<[u8; MCE_PK_LEN]> = raw[1 + MLKEM_EK_LEN..1 + MLKEM_EK_LEN + MCE_PK_LEN]
        .to_vec()
        .into_boxed_slice()
        .try_into()
        .ok()?;
    let x: [u8; 32] = raw[1 + MLKEM_EK_LEN + MCE_PK_LEN..].try_into().ok()?;
    Some((ek, mce.into(), x25519_dalek::PublicKey::from(x)))
}

/// Seals `opening` for the holder of `bundle`, which must match `addr`.
pub fn seal(addr: &Address, bundle: &Bundle, opening: &Opening) -> Result<String, String> {
    if bundle.hash() != addr.bundle_hash {
        return Err("the bundle does not match the address".into());
    }
    let (ek, mce_pk, x_pk) = parse_bundle(bundle).ok_or("malformed bundle")?;
    let (ct1, ss1) = ek.encapsulate(&mut OsRng).map_err(|_| "ML-KEM encapsulation failed")?;
    let (ct2, ss2) = classic_mceliece_rust::encapsulate_boxed(&mce_pk, &mut OsRng);
    let esk = x25519_dalek::EphemeralSecret::random_from_rng(OsRng);
    let epk = x25519_dalek::PublicKey::from(&esk);
    let ss3 = esk.diffie_hellman(&x_pk);
    let (k, commit) = kdf(
        [ss1.as_slice(), ss2.as_array(), ss3.as_bytes()],
        [ct1.as_slice(), ct2.as_array(), epk.as_bytes()],
        &addr.bundle_hash,
    );
    // The key is fresh per blob, so the fixed nonce is never reused.
    let aead = ChaCha20Poly1305::new(&k.into())
        .encrypt(&[0u8; 12].into(), &opening.to_bytes()[..])
        .map_err(|_| "AEAD failed")?;
    let mut blob = vec![1u8];
    blob.extend_from_slice(ct1.as_slice());
    blob.extend_from_slice(ct2.as_array());
    blob.extend_from_slice(epk.as_bytes());
    blob.extend_from_slice(&commit);
    blob.extend_from_slice(&aead);
    debug_assert_eq!(blob.len(), SEALED_LEN);
    Ok(format!("mzx1{}", base32(&blob)))
}

/// Opens a sealed blob with the delivery keys of address `j`, or `None` if
/// it is not for that address (or was tampered with).
pub fn open(seed: &[u8; 32], j: u32, bundle_hash: &[u8; 32], sealed: &str) -> Option<Opening> {
    let blob = unbase32(sealed.trim().strip_prefix("mzx1")?)?;
    if blob.len() != SEALED_LEN || blob[0] != 1 {
        return None;
    }
    let keys = DeliveryKeys::derive(seed, j);
    let mut off = 1;
    let ct1 = &blob[off..off + MLKEM_CT_LEN];
    off += MLKEM_CT_LEN;
    let ct2: [u8; MCE_CT_LEN] = blob[off..off + MCE_CT_LEN].try_into().ok()?;
    off += MCE_CT_LEN;
    let epk: [u8; 32] = blob[off..off + 32].try_into().ok()?;
    off += 32;
    let commit: [u8; 32] = blob[off..off + 32].try_into().ok()?;
    off += 32;
    let ss1 = keys.mlkem_dk.decapsulate(&ct1.try_into().ok()?).ok()?;
    let ss2 = classic_mceliece_rust::decapsulate_boxed(&ct2.into(), &keys.mce_sk);
    let ss3 = keys.x_sk.diffie_hellman(&x25519_dalek::PublicKey::from(epk));
    let (k, expected) =
        kdf([ss1.as_slice(), ss2.as_array(), ss3.as_bytes()], [ct1, &ct2, &epk], bundle_hash);
    if expected != commit {
        return None;
    }
    let plain = ChaCha20Poly1305::new(&k.into()).decrypt(&[0u8; 12].into(), &blob[off..]).ok()?;
    Opening::from_bytes(&plain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use p3_field::PrimeCharacteristicRing;

    fn opening() -> Opening {
        Opening {
            d: [F::new(1), F::new(2), F::new(3)],
            value: 42,
            psi: std::array::from_fn(|i| F::new(i as u32)),
            y: std::array::from_fn(|i| F::new(1000 + i as u32)),
        }
    }

    #[test]
    fn addresses_and_plain_openings_round_trip() {
        let sponge = Sponge::new(9);
        let keys = WalletKeys::from_seed(&sponge, [3; 32]);
        let (addr, bundle) = address(&keys, &sponge, 0);
        assert_eq!(Address::decode(&addr.encode()), Some(addr.clone()));
        assert_eq!(bundle.hash(), addr.bundle_hash);
        assert_eq!(Opening::from_plain(&opening().to_plain()), Some(opening()));
        let _ = F::ZERO;
    }

    #[test]
    fn sealed_openings_open_only_for_their_recipient() {
        let sponge = Sponge::new(9);
        let bob = WalletKeys::from_seed(&sponge, [4; 32]);
        let carol = WalletKeys::from_seed(&sponge, [5; 32]);
        let (addr, bundle) = address(&bob, &sponge, 0);
        let sealed = seal(&addr, &bundle, &opening()).unwrap();
        assert_eq!(unbase32(&sealed[4..]).unwrap().len(), SEALED_LEN);
        assert_eq!(open(&bob.seed, 0, &addr.bundle_hash, &sealed), Some(opening()));
        assert_eq!(open(&carol.seed, 0, &addr.bundle_hash, &sealed), None, "another wallet");
        assert_eq!(open(&bob.seed, 1, &addr.bundle_hash, &sealed), None, "another address");
        // Tampering anywhere fails the key commitment or the AEAD.
        let mut raw = unbase32(&sealed[4..]).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 1;
        assert_eq!(open(&bob.seed, 0, &addr.bundle_hash, &format!("mzx1{}", base32(&raw))), None);
        // A bundle that does not match the address is refused.
        let (_, carol_bundle) = address(&carol, &sponge, 0);
        assert!(seal(&addr, &carol_bundle, &opening()).is_err());
    }
}
