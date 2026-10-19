//! The "sealed" opening (spec §9.2–9.3): a note opening encrypted to the
//! recipient's bundle, for transport over any third-party channel.
//!
//! - Hybrid KEM: ML-KEM-768 (module lattices) + Classic McEliece-460896
//!   (Goppa codes) + X25519. The combiner hashes all three secrets and all
//!   ciphertexts, so the blob stays secret while any one component holds.
//! - Anonymous: there is no sender signature and no sender identifier. Anyone
//!   holding the bundle could have produced it, which gives deniability.
//! - Key-committing: a hash of the key precedes the AEAD ciphertext, so a blob
//!   can't be made to decrypt under two different keys.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::ChaCha20Poly1305;
use ml_kem::kem::{Decapsulate, Encapsulate};
use ml_kem::EncodedSizeUser;
use rand::rngs::OsRng;

use crate::keys::*;
use crate::note::{NoteOpening, OPENING_LEN};
use crate::primitives::*;

pub const MLKEM_CT_LEN: usize = 1088;
pub const MCE_CT_LEN: usize = classic_mceliece_rust::CRYPTO_CIPHERTEXTBYTES;
pub const SEALED_LEN: usize = 1 + MLKEM_CT_LEN + MCE_CT_LEN + 32 + 32 + OPENING_LEN + 16;

fn kdf(ss: [&[u8]; 3], cts: [&[u8]; 3], bundle_hash: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let k: [u8; 32] =
        Xof::new("mazze/seal/kdf/v1", &[ss[0], ss[1], ss[2], cts[0], cts[1], cts[2], bundle_hash]).bytes();
    let commit: [u8; 32] = Xof::new("mazze/seal/key-commit/v1", &[&k]).bytes();
    (k, commit)
}

/// Split a bundle into its three public keys, checking its length and version.
fn parse_bundle(b: &Bundle) -> Option<(MlKemEk, classic_mceliece_rust::PublicKey<'static>, x25519_dalek::PublicKey)> {
    let raw = &b.0;
    if raw.len() != 1 + MLKEM_EK_LEN + MCE_PK_LEN + 32 || raw[0] != 1 {
        return None;
    }
    let ek_bytes = &raw[1..1 + MLKEM_EK_LEN];
    let ek = MlKemEk::from_bytes(ek_bytes.try_into().ok()?);
    let mce: Box<[u8; MCE_PK_LEN]> =
        raw[1 + MLKEM_EK_LEN..1 + MLKEM_EK_LEN + MCE_PK_LEN].to_vec().into_boxed_slice().try_into().ok()?;
    let x: [u8; 32] = raw[1 + MLKEM_EK_LEN + MCE_PK_LEN..].try_into().ok()?;
    Some((ek, mce.into(), x25519_dalek::PublicKey::from(x)))
}

/// Seal an opening for the holder of `bundle`. The bundle must match the
/// recipient address's `bundle_hash`.
pub fn seal(addr: &Address, bundle: &Bundle, opening: &NoteOpening) -> Result<Vec<u8>, String> {
    if bundle.hash() != addr.bundle_hash {
        return Err("bundle does not match the address".into());
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
    Ok(blob)
}

/// Try to open a sealed blob with the delivery keys of one of my addresses.
pub fn open(keys: &DeliveryKeys, bundle_hash: &[u8; 32], blob: &[u8]) -> Option<NoteOpening> {
    if blob.len() != SEALED_LEN || blob[0] != 1 {
        return None;
    }
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
    let (k, expect_commit) =
        kdf([ss1.as_slice(), ss2.as_array(), ss3.as_bytes()], [ct1, &ct2, &epk], bundle_hash);
    if expect_commit != commit {
        return None; // not for me (or tampered)
    }
    let pt = ChaCha20Poly1305::new(&k.into()).decrypt(&[0u8; 12].into(), &blob[off..]).ok()?;
    NoteOpening::from_bytes(&pt)
}
