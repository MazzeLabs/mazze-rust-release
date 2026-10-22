//! The wallet file: the seed, the rebuilt tree, and the note openings.
//!
//! On disk it is encrypted as a whole (the openings reveal notes as much as
//! the seed does): a key is derived from a passphrase with Argon2id and the
//! wallet is sealed with XChaCha20-Poly1305, a fresh nonce on every save and
//! the header bound as associated data. Both are symmetric, so the file is
//! as safe against a quantum adversary as the passphrase is long.

use mazze_shielded_pq::{
    field::{Digest, Sponge, DIGEST, F},
    note::{Note, HM_L},
};
use p3_field::PrimeField32;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

#[derive(Serialize, Deserialize)]
pub struct Wallet {
    /// The key the file is sealed with, derived once per run.
    #[serde(skip)]
    pub(crate) seal: Option<Seal>,
    #[serde(with = "hex32")]
    pub seed: [u8; 32],
    /// The next epoch to fetch.
    pub synced_epoch: u64,
    /// Every leaf of the commitment tree, in position order.
    pub leaves: Vec<[u32; DIGEST]>,
    pub notes: Vec<StoredNote>,
    /// The next self-note index (spec §9.5).
    #[serde(default)]
    pub next_self: u32,
    /// Proven transactions not yet seen on chain, kept so that a crash or a
    /// lost submission after a deposit never loses the value (`resubmit`).
    #[serde(default)]
    pub pending: Vec<PendingTx>,
    /// Receiving addresses issued so far: indices `0..next_address`.
    #[serde(default = "one")]
    pub next_address: u32,
}

fn one() -> u32 { 1 }

/// A proven transaction and the commitment of the wallet's own output in
/// it: the entry is dropped once that commitment appears in the tree.
#[derive(Serialize, Deserialize, Clone)]
pub struct PendingTx {
    pub body: String,
    pub proof: String,
    pub cm: [u32; DIGEST],
}

/// A note opening: what the owner needs to spend it. As written to a file
/// for the recipient, it is the out-of-band delivery of a payment.
#[derive(Serialize, Deserialize, Clone)]
pub struct StoredNote {
    pub d: [u32; 3],
    pub value: u64,
    pub psi: Vec<u32>,
    pub y: Vec<u32>,
    pub cm: [u32; DIGEST],
    #[serde(default)]
    pub position: Option<u64>,
    #[serde(default)]
    pub spent: bool,
    /// Set for the wallet's own notes, which the seed alone can recover.
    #[serde(default)]
    pub self_index: Option<u32>,
}

fn elems<const N: usize>(xs: &[u32]) -> [F; N] { std::array::from_fn(|i| F::new(xs[i])) }

impl StoredNote {
    pub fn from_note(note: &Note, sponge: &Sponge) -> Self {
        StoredNote {
            d: note.d.map(|x| x.as_canonical_u32()),
            value: note.value,
            psi: note.psi.iter().map(|x| x.as_canonical_u32()).collect(),
            y: note.y.iter().map(|x| x.as_canonical_u32()).collect(),
            cm: note.cm(sponge).map(|x| x.as_canonical_u32()),
            position: None,
            spent: false,
            self_index: None,
        }
    }

    pub fn d(&self) -> [F; 3] { elems(&self.d) }

    pub fn psi(&self) -> Digest { elems::<DIGEST>(&self.psi) }

    pub fn y(&self) -> [F; HM_L] { elems::<HM_L>(&self.y) }
}

impl Wallet {
    /// A new wallet, sealed under `passphrase` from its first save.
    pub fn new(seed: [u8; 32], passphrase: &str) -> Result<Self, String> {
        let mut salt = [0u8; 16];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut salt);
        let kdf = Kdf { m_kib: KDF_M_KIB, t: KDF_T, p: KDF_P, salt: hex::encode(salt) };
        Ok(Wallet {
            seal: Some(Seal::derive(kdf, passphrase)?),
            seed,
            synced_epoch: 0,
            leaves: Vec::new(),
            notes: Vec::new(),
            next_self: 0,
            pending: Vec::new(),
            next_address: 1,
        })
    }

    /// Opens a wallet file; `passphrase` is asked for only when needed.
    pub fn load(path: &str, passphrase: impl FnOnce() -> Result<Zeroizing<String>, String>) -> Result<Self, String> {
        let text = Zeroizing::new(std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?);
        let envelope: Envelope = serde_json::from_str(&text)
            .map_err(|_| format!("{path}: not a sealed wallet file ({FORMAT})"))?;
        if envelope.format != FORMAT {
            return Err(format!("{path}: unknown format {}", envelope.format));
        }
        let seal = Seal::derive(envelope.kdf.clone(), &passphrase()?)?;
        let plain = seal.open(&envelope)?;
        let mut w: Wallet = serde_json::from_slice(&plain).map_err(|e| format!("{path}: {e}"))?;
        w.seal = Some(seal);
        Ok(w)
    }

    /// Written atomically and readable only by the owner: a crash never
    /// leaves a half-written wallet.
    pub fn save(&self, path: &str) -> Result<(), String> {
        let seal = self.seal.as_ref().ok_or("the wallet has no key")?;
        let plain = Zeroizing::new(serde_json::to_vec(self).map_err(|e| e.to_string())?);
        let envelope = seal.close(&plain)?;
        let tmp = format!("{path}.tmp");
        {
            use std::{io::Write, os::unix::fs::OpenOptionsExt};
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .map_err(|e| e.to_string())?;
            f.write_all(serde_json::to_string(&envelope).unwrap().as_bytes())
                .and_then(|_| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }
}

/// The file format; also the associated data, with the KDF parameters.
const FORMAT: &str = "mazze-shielded-wallet/1";

/// Argon2id: 256 MiB, 3 passes (about a second), so a stolen file costs an
/// attacker that much per guess.
const KDF_M_KIB: u32 = 256 * 1024;
const KDF_T: u32 = 3;
const KDF_P: u32 = 1;

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct Kdf {
    m_kib: u32,
    t: u32,
    p: u32,
    salt: String,
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    format: String,
    kdf: Kdf,
    nonce: String,
    ciphertext: String,
}

pub(crate) struct Seal {
    kdf: Kdf,
    key: Zeroizing<[u8; 32]>,
}

impl Seal {
    fn derive(kdf: Kdf, passphrase: &str) -> Result<Self, String> {
        if passphrase.is_empty() {
            return Err("the passphrase is empty".into());
        }
        let salt = hex::decode(&kdf.salt).map_err(|e| e.to_string())?;
        let params = argon2::Params::new(kdf.m_kib, kdf.t, kdf.p, Some(32)).map_err(|e| e.to_string())?;
        let mut key = Zeroizing::new([0u8; 32]);
        argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
            .hash_password_into(passphrase.as_bytes(), &salt, &mut *key)
            .map_err(|e| e.to_string())?;
        Ok(Seal { kdf, key })
    }

    fn aad(&self) -> Vec<u8> {
        format!("{FORMAT}|{}|{}|{}|{}", self.kdf.m_kib, self.kdf.t, self.kdf.p, self.kdf.salt).into_bytes()
    }

    fn close(&self, plain: &[u8]) -> Result<Envelope, String> {
        use chacha20poly1305::{aead::{Aead, KeyInit, Payload}, XChaCha20Poly1305, XNonce};
        let mut nonce = [0u8; 24];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut nonce);
        let cipher = XChaCha20Poly1305::new((&*self.key).into());
        let ct = cipher
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad: &self.aad() })
            .map_err(|_| "sealing the wallet failed")?;
        Ok(Envelope {
            format: FORMAT.into(),
            kdf: self.kdf.clone(),
            nonce: hex::encode(nonce),
            ciphertext: hex::encode(ct),
        })
    }

    fn open(&self, envelope: &Envelope) -> Result<Zeroizing<Vec<u8>>, String> {
        use chacha20poly1305::{aead::{Aead, KeyInit, Payload}, XChaCha20Poly1305, XNonce};
        let nonce = hex::decode(&envelope.nonce).map_err(|e| e.to_string())?;
        let ct = hex::decode(&envelope.ciphertext).map_err(|e| e.to_string())?;
        if nonce.len() != 24 {
            return Err("bad nonce".into());
        }
        let cipher = XChaCha20Poly1305::new((&*self.key).into());
        cipher
            .decrypt(XNonce::from_slice(&nonce), Payload { msg: &ct, aad: &self.aad() })
            .map(Zeroizing::new)
            .map_err(|_| "wrong passphrase, or the wallet file was altered".into())
    }
}

mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        let v = hex::decode(s).map_err(serde::de::Error::custom)?;
        v.try_into().map_err(|_| serde::de::Error::custom("seed must be 32 bytes"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_round_trip_wrong_passphrase_and_tampering() {
        let dir = std::env::temp_dir().join(format!("mzw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("w.json").to_string_lossy().to_string();
        let mut w = Wallet::new([7u8; 32], "correct horse").unwrap();
        w.next_self = 5;
        w.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(&hex::encode([7u8; 32])), "the seed is not on disk in clear");
        let back = Wallet::load(&path, || Ok(Zeroizing::new("correct horse".into()))).unwrap();
        assert_eq!((back.seed, back.next_self), ([7u8; 32], 5));
        assert!(Wallet::load(&path, || Ok(Zeroizing::new("wrong".into()))).is_err());
        // Any change to the header (here the KDF cost) is caught.
        let tampered = text.replace("\"t\":3", "\"t\":1");
        std::fs::write(&path, tampered).unwrap();
        assert!(Wallet::load(&path, || Ok(Zeroizing::new("correct horse".into()))).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
