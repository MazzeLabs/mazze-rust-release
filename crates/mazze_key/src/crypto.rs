// Copyright 2015-2019 Parity Technologies (UK) Ltd.
// This file is part of Parity Ethereum.

// Parity Ethereum is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity Ethereum is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity Ethereum.  If not, see <http://www.gnu.org/licenses/>.

use secp256k1;
use std::{fmt, io};

/// String-carrying error variant covering AES-CTR + HMAC failures from
/// the RustCrypto crates. Kept as a single variant so the public
/// `crypto::Error` surface stays stable for callers.
#[derive(Debug)]
pub struct SymmError(pub String);

impl fmt::Display for SymmError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "symmetric crypto error: {}", self.0)
    }
}

impl std::error::Error for SymmError {}

quick_error! {
    #[derive(Debug)]
    pub enum Error {
        Secp(e: secp256k1::Error) {
            display("secp256k1 error: {}", e)
            cause(e)
            from()
        }
        Io(e: io::Error) {
            display("i/o error: {}", e)
            cause(e)
            from()
        }
        InvalidMessage {
            display("invalid message")
        }
        Symm(e: SymmError) {
            cause(e)
            from()
        }
    }
}

/// ECDH functions
pub mod ecdh {
    use super::Error;
    use crate::Public;
    use crate::Secret;
    use secp256k1::{self, ecdh::SharedSecret, PublicKey, SecretKey};

    /// Agree on a shared secret. Used by the ECIES handshake below
    /// and by the network-layer peer handshake.
    pub fn agree(secret: &Secret, public: &Public) -> Result<Secret, Error> {
        let pdata = {
            let mut temp = [4u8; 65];
            (&mut temp[1..65]).copy_from_slice(&public[0..64]);
            temp
        };

        let publ = PublicKey::from_slice(&pdata)?;
        let sec = SecretKey::from_slice(secret.as_bytes())?;
        // Upstream `secp256k1::ecdh::SharedSecret::new` takes no
        // context (the FFI handles secp init internally).
        let shared = SharedSecret::new(&publ, &sec);

        Secret::from_unsafe_slice(&shared[0..32])
            .map_err(|_| Error::Secp(secp256k1::Error::InvalidSecretKey))
    }
}

/// ECIES function
pub mod ecies {
    use super::{ecdh, Error, SymmError};
    use crate::Generator;
    use crate::Public;
    use crate::Random;
    use crate::Secret;
    use aes::cipher::{KeyIvInit, StreamCipher};
    use hmac::{Hmac, Mac};
    use mazze_types::H128;
    use sha2::{Digest, Sha256};
    use subtle::ConstantTimeEq;

    type Aes128Ctr = ctr::Ctr64BE<aes::Aes128>;
    type HmacSha256 = Hmac<Sha256>;

    /// Encrypt a message with a public key, writing an HMAC covering both
    /// the plaintext and authenticated data.
    ///
    /// Authenticated data may be empty.
    pub fn encrypt(
        public: &Public, auth_data: &[u8], plain: &[u8],
    ) -> Result<Vec<u8>, Error> {
        let r = Random.generate()?;
        let z = ecdh::agree(r.secret(), public)?;
        let mut key = [0u8; 32];
        kdf(&z, &[0u8; 0], &mut key);

        let ekey = &key[0..16];
        let mkey_seed = Sha256::digest(&key[16..32]);

        let mut msg = vec![0u8; 1 + 64 + 16 + plain.len() + 32];
        msg[0] = 0x04u8;
        {
            let msgd = &mut msg[1..];
            msgd[0..64].copy_from_slice(r.public().as_bytes());
            let iv = H128::random();
            msgd[64..80].copy_from_slice(iv.as_bytes());
            // AES-128-CTR in place.
            {
                let cipher_buf =
                    &mut msgd[(64 + 16)..(64 + 16 + plain.len())];
                cipher_buf.copy_from_slice(plain);
                let mut cipher = Aes128Ctr::new(ekey.into(), iv.as_bytes().into());
                cipher.apply_keystream(cipher_buf);
            }
            // HMAC over (IV || ciphertext || auth_data). parity_crypto
            // built this as: hmac.update(cipher_iv) + hmac.update(auth_data)
            // where cipher_iv = msgd[64..64+16+plain_len]. We replicate
            // exactly.
            let mut hmac = <HmacSha256 as Mac>::new_from_slice(&mkey_seed)
                .map_err(|e| SymmError(format!("hmac init: {}", e)))?;
            hmac.update(&msgd[64..(64 + 16 + plain.len())]);
            hmac.update(auth_data);
            let sig = hmac.finalize().into_bytes();
            msgd[(64 + 16 + plain.len())..].copy_from_slice(&sig);
        }
        Ok(msg)
    }

    /// Decrypt a message with a secret key, checking HMAC for ciphertext
    /// and authenticated data validity.
    pub fn decrypt(
        secret: &Secret, auth_data: &[u8], encrypted: &[u8],
    ) -> Result<Vec<u8>, Error> {
        let meta_len = 1 + 64 + 16 + 32;
        if encrypted.len() < meta_len || encrypted[0] < 2 || encrypted[0] > 4 {
            return Err(Error::InvalidMessage); //invalid message: publickey
        }

        let e = &encrypted[1..];
        let p = Public::from_slice(&e[0..64]);
        let z = ecdh::agree(secret, &p)?;
        let mut key = [0u8; 32];
        kdf(&z, &[0u8; 0], &mut key);

        let ekey = &key[0..16];
        let mkey_seed = Sha256::digest(&key[16..32]);

        let clen = encrypted.len() - meta_len;
        let cipher_with_iv = &e[64..(64 + 16 + clen)];
        let cipher_iv = &cipher_with_iv[0..16];
        let cipher_no_iv = &cipher_with_iv[16..];
        let msg_mac = &e[(64 + 16 + clen)..];

        // Verify tag.
        let mut hmac = <HmacSha256 as Mac>::new_from_slice(&mkey_seed)
            .map_err(|e| SymmError(format!("hmac init: {}", e)))?;
        hmac.update(cipher_with_iv);
        hmac.update(auth_data);
        let mac = hmac.finalize().into_bytes();

        if mac.as_slice().ct_eq(msg_mac).unwrap_u8() != 1 {
            return Err(Error::InvalidMessage);
        }

        // AES-128-CTR decrypt (CTR is symmetric).
        let mut msg = vec![0u8; clen];
        msg.copy_from_slice(cipher_no_iv);
        let mut cipher = Aes128Ctr::new(ekey.into(), cipher_iv.into());
        cipher.apply_keystream(&mut msg);
        Ok(msg)
    }

    fn kdf(secret: &Secret, s1: &[u8], dest: &mut [u8]) {
        // NIST SP 800-56A § 5.8.1 (Concatenation KDF). 4-byte big-endian
        // counter, then secret, then optional s1, all fed into SHA-256.
        // Output dest[0..len] filled in 32-byte SHA-256 blocks.
        let mut ctr = 1u32;
        let mut written = 0usize;
        while written < dest.len() {
            let mut hasher = Sha256::new();
            let ctrs = [
                (ctr >> 24) as u8,
                (ctr >> 16) as u8,
                (ctr >> 8) as u8,
                ctr as u8,
            ];
            hasher.update(&ctrs);
            hasher.update(secret.as_bytes());
            hasher.update(s1);
            let d = hasher.finalize();
            dest[written..(written + 32)].copy_from_slice(&d);
            written += 32;
            ctr += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ecies;
    use crate::Generator;
    use crate::Random;

    #[test]
    fn ecies_shared() {
        let kp = Random.generate().unwrap();
        let message = b"So many books, so little time";

        let shared = b"shared";
        let wrong_shared = b"incorrect";
        let encrypted = ecies::encrypt(kp.public(), shared, message).unwrap();
        assert!(encrypted[..] != message[..]);
        assert_eq!(encrypted[0], 0x04);

        assert!(ecies::decrypt(kp.secret(), wrong_shared, &encrypted).is_err());
        let decrypted =
            ecies::decrypt(kp.secret(), shared, &encrypted).unwrap();
        assert_eq!(decrypted[..message.len()], message[..]);
    }
}
