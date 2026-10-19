// Copyright 2025 Mazze Foundation
//
// Local crypto module backing the V3 keystore + presale wallet on-disk
// formats. Wire format (must not change without a new keystore version):
//   * V3 keystore: PBKDF2-SHA256 + Scrypt + AES-128-CTR + HMAC-SHA256
//                  with MAC = keccak256(right_bits || ciphertext).
//   * Presale wallet: PBKDF2-SHA256 + AES-128-CBC.

use sha2::{Digest, Sha256};

pub const KEY_LENGTH: usize = 32;
pub const KEY_ITERATIONS: usize = 10240;
pub const KEY_LENGTH_AES: usize = 16;

/// Error covering the symmetric-crypto failure modes parity_crypto
/// exposed via `parity_crypto::error::SymmError`. Errors here are
/// non-fatal at the chain layer (they surface as RPC / keystore
/// failures), so a string payload is sufficient.
#[derive(Debug)]
pub struct SymmError(pub String);

impl std::fmt::Display for SymmError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "symmetric crypto error: {}", self.0)
    }
}

impl std::error::Error for SymmError {}

/// Error for Scrypt parameter / derivation failures (parity_crypto
/// exposed `parity_crypto::error::ScryptError`).
#[derive(Debug)]
pub struct ScryptError(pub String);

impl std::fmt::Display for ScryptError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "scrypt error: {}", self.0)
    }
}

impl std::error::Error for ScryptError {}

/// Combined error matching parity_crypto's `crypto::Error`.
#[derive(Debug)]
pub enum Error {
    Symm(SymmError),
    Scrypt(ScryptError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Symm(e) => write!(f, "{}", e),
            Error::Scrypt(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Symm(e) => Some(e),
            Error::Scrypt(e) => Some(e),
        }
    }
}

impl From<SymmError> for Error {
    fn from(e: SymmError) -> Self {
        Error::Symm(e)
    }
}

impl From<ScryptError> for Error {
    fn from(e: ScryptError) -> Self {
        Error::Scrypt(e)
    }
}

/// Re-export sub-namespace for error types — preserves the
/// `crypto::error::SymmError` / `crypto::error::ScryptError` paths
/// the previous code used.
pub mod error {
    pub use super::{ScryptError, SymmError};
}

/// Keccak-256 trait that mirrors parity_crypto's `Keccak256` so call
/// sites like `right_bits.keccak256()` keep working unchanged.
pub trait Keccak256<T> {
    fn keccak256(&self) -> T
    where
        T: Sized;
}

impl<T: AsRef<[u8]> + ?Sized> Keccak256<[u8; 32]> for T {
    fn keccak256(&self) -> [u8; 32] {
        use tiny_keccak::{Hasher, Keccak};
        let mut keccak = Keccak::v256();
        let mut result = [0u8; 32];
        keccak.update(self.as_ref());
        keccak.finalize(&mut result);
        result
    }
}

/// Constant-time slice equality. Replaces `parity_crypto::is_equal`.
pub fn is_equal(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).unwrap_u8() == 1
}

/// Concatenate `derived_right_bits || ciphertext` for the V3
/// keystore MAC. The result is then keccak256-hashed by the caller.
pub fn derive_mac(derived_right_bits: &[u8], ciphertext: &[u8]) -> Vec<u8> {
    let mut mac = Vec::with_capacity(derived_right_bits.len() + ciphertext.len());
    mac.extend_from_slice(derived_right_bits);
    mac.extend_from_slice(ciphertext);
    mac
}

/// PBKDF2-SHA256 with the V3 keystore convention: 32-byte derived key
/// split into two 16-byte halves (`derived_left_bits` for AES-128-CTR
/// key, `derived_right_bits` for MAC).
pub fn derive_key_iterations(
    password: &[u8], salt: &[u8], iterations: u32,
) -> (Vec<u8>, Vec<u8>) {
    let mut derived = [0u8; KEY_LENGTH];
    // Absolute path `::pbkdf2` so our local `pub mod pbkdf2` (which
    // shadows the crate name in this module) doesn't shadow the
    // function we want from the actual crate. Infallible — fills
    // the whole buffer.
    ::pbkdf2::pbkdf2_hmac::<Sha256>(password, salt, iterations, &mut derived);
    let left = derived[0..KEY_LENGTH_AES].to_vec();
    let right = derived[KEY_LENGTH_AES..KEY_LENGTH].to_vec();
    (left, right)
}

/// AES-128-CTR primitives matching parity_crypto's `aes` module.
pub mod aes {
    use super::SymmError;
    use aes::cipher::{
        block_padding::Pkcs7, BlockDecryptMut, KeyIvInit, StreamCipher,
    };

    type Aes128Ctr = ctr::Ctr64BE<aes::Aes128>;
    type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

    /// Encrypts `plain` to `dest` with AES-128-CTR. Both buffers must
    /// be the same length.
    pub fn encrypt_128_ctr(
        key: &[u8], iv: &[u8], plain: &[u8], dest: &mut [u8],
    ) -> Result<(), SymmError> {
        if plain.len() != dest.len() {
            return Err(SymmError(format!(
                "encrypt_128_ctr: plain.len={} != dest.len={}",
                plain.len(),
                dest.len()
            )));
        }
        if key.len() != 16 || iv.len() != 16 {
            return Err(SymmError(format!(
                "encrypt_128_ctr: bad key/iv length ({}/{}); expected 16/16",
                key.len(),
                iv.len()
            )));
        }
        dest.copy_from_slice(plain);
        let mut cipher = Aes128Ctr::new(key.into(), iv.into());
        cipher.apply_keystream(dest);
        Ok(())
    }

    /// Decrypts `cipher_input` to `dest` with AES-128-CTR. Both
    /// buffers must be the same length. CTR is symmetric, so this is
    /// effectively the same as `encrypt_128_ctr` — but the function
    /// is kept distinct to preserve the call-site readability.
    pub fn decrypt_128_ctr(
        key: &[u8], iv: &[u8], cipher_input: &[u8], dest: &mut [u8],
    ) -> Result<(), SymmError> {
        if cipher_input.len() != dest.len() {
            return Err(SymmError(format!(
                "decrypt_128_ctr: cipher.len={} != dest.len={}",
                cipher_input.len(),
                dest.len()
            )));
        }
        if key.len() != 16 || iv.len() != 16 {
            return Err(SymmError(format!(
                "decrypt_128_ctr: bad key/iv length ({}/{}); expected 16/16",
                key.len(),
                iv.len()
            )));
        }
        dest.copy_from_slice(cipher_input);
        let mut cipher = Aes128Ctr::new(key.into(), iv.into());
        cipher.apply_keystream(dest);
        Ok(())
    }

    /// Decrypts `cipher_input` to `dest` with AES-128-CBC (PKCS7
    /// padding). Returns the number of plaintext bytes written.
    /// Used only by the Ethereum presale wallet decoder.
    pub fn decrypt_128_cbc(
        key: &[u8], iv: &[u8], cipher_input: &[u8], dest: &mut [u8],
    ) -> Result<usize, SymmError> {
        if key.len() != 16 || iv.len() != 16 {
            return Err(SymmError(format!(
                "decrypt_128_cbc: bad key/iv length ({}/{}); expected 16/16",
                key.len(),
                iv.len()
            )));
        }
        if dest.len() < cipher_input.len() {
            return Err(SymmError(format!(
                "decrypt_128_cbc: dest too small ({} < {})",
                dest.len(),
                cipher_input.len()
            )));
        }
        let cipher = Aes128CbcDec::new(key.into(), iv.into());
        let plain = cipher
            .decrypt_padded_b2b_mut::<Pkcs7>(cipher_input, dest)
            .map_err(|e| SymmError(format!("decrypt_128_cbc: {}", e)))?;
        Ok(plain.len())
    }
}

/// Scrypt KDF matching the V3 keystore convention: derives 32 bytes,
/// returns the two 16-byte halves like `derive_key_iterations`.
pub mod scrypt {
    use super::ScryptError;
    use ::scrypt::Params;

    /// V3 keystore Scrypt: `n` is the actual N parameter (must be a
    /// power of two). The `scrypt` crate's `Params::new` takes `log2(N)`.
    pub fn derive_key(
        password: &[u8], salt: &[u8], n: u32, p: u32, r: u32,
    ) -> Result<(Vec<u8>, Vec<u8>), ScryptError> {
        if !n.is_power_of_two() || n < 2 {
            return Err(ScryptError(format!(
                "scrypt N must be a power of two ≥ 2 (got {})",
                n
            )));
        }
        let log_n = n.trailing_zeros() as u8;
        let params = Params::new(log_n, r, p, 32)
            .map_err(|e| ScryptError(format!("invalid scrypt params: {}", e)))?;
        let mut derived = [0u8; 32];
        ::scrypt::scrypt(password, salt, &params, &mut derived)
            .map_err(|e| ScryptError(format!("scrypt derive: {}", e)))?;
        Ok((derived[0..16].to_vec(), derived[16..32].to_vec()))
    }
}

/// PBKDF2 helpers matching the legacy Ethereum **presale wallet**
/// API (`parity_crypto::pbkdf2`). The presale flow uses 32-byte
/// derived keys with full PBKDF2-SHA256; the V3 keystore goes
/// through `derive_key_iterations` instead.
pub mod pbkdf2 {
    pub struct Salt<'a>(pub &'a [u8]);
    pub struct Secret<'a>(pub &'a [u8]);

    /// Fill `output` with PBKDF2-SHA256(secret, salt, iter). Output
    /// length is determined by the caller's buffer size.
    pub fn sha256(
        iter: u32, salt: Salt<'_>, secret: Secret<'_>, output: &mut [u8],
    ) {
        // Absolute path to the crate-level `pbkdf2` function. (We're
        // inside a sub-module named `pbkdf2`; without the leading
        // `::` Rust resolves the name to this module.) Infallible.
        ::pbkdf2::pbkdf2_hmac::<sha2::Sha256>(secret.0, salt.0, iter, output);
    }
}
