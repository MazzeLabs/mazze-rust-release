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

use crate::Error;
use malloc_size_of_derive::MallocSizeOf as DeriveMallocSizeOf;
use mazze_types::H256;
use secp256k1::{constants::SECRET_KEY_SIZE as SECP256K1_SECRET_KEY_SIZE, SecretKey};
use std::{fmt, ops::Deref, str::FromStr};
use zeroize::Zeroize;

#[derive(Clone, PartialEq, Eq, DeriveMallocSizeOf)]
pub struct Secret {
    inner: H256,
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.inner.0.zeroize()
    }
}

impl fmt::LowerHex for Secret {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        self.inner.fmt(fmt)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        self.inner.fmt(fmt)
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(
            fmt,
            "Secret: 0x{:x}{:x}..{:x}{:x}",
            self.inner[0], self.inner[1], self.inner[30], self.inner[31]
        )
    }
}

impl Secret {
    /// Creates a `Secret` from the given slice, returning `None` if the slice
    /// length != 32. Does NOT validate as a secp256k1 scalar — use
    /// `from_unsafe_slice` for that.
    pub fn from_slice(key: &[u8]) -> Option<Self> {
        if key.len() != 32 {
            return None;
        }
        let mut h = H256::zero();
        h.as_bytes_mut().copy_from_slice(&key[0..32]);
        Some(Secret { inner: h })
    }

    /// Creates zero key, which is invalid for crypto operations.
    /// Retained for compatibility with callers that need a sentinel.
    pub fn zero() -> Self {
        Secret {
            inner: H256::zero(),
        }
    }

    /// Imports and validates the key.
    pub fn from_unsafe_slice(key: &[u8]) -> Result<Self, Error> {
        let secret = SecretKey::from_slice(key)?;
        Ok(secret.into())
    }

    /// Checks validity of this key as a secp256k1 scalar.
    pub fn check_validity(&self) -> Result<(), Error> {
        self.to_secp256k1_secret().map(|_| ())
    }

    /// Create `secp256k1::SecretKey` based on this secret.
    pub fn to_secp256k1_secret(&self) -> Result<SecretKey, Error> {
        Ok(SecretKey::from_slice(&self[..])?)
    }

    pub fn to_hex(&self) -> String {
        format!("{:x}", self.inner)
    }
}

impl FromStr for Secret {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(H256::from_str(s)
            .map_err(|e| Error::Custom(format!("{:?}", e)))?
            .into())
    }
}

impl From<[u8; 32]> for Secret {
    fn from(k: [u8; 32]) -> Self {
        Secret { inner: H256(k) }
    }
}

impl From<H256> for Secret {
    fn from(s: H256) -> Self {
        s.0.into()
    }
}

impl From<&'static str> for Secret {
    fn from(s: &'static str) -> Self {
        s.parse().unwrap_or_else(|_| {
            panic!("invalid string literal for {}: '{}'", stringify!(Self), s)
        })
    }
}

impl From<SecretKey> for Secret {
    fn from(key: SecretKey) -> Self {
        let mut a = [0u8; SECP256K1_SECRET_KEY_SIZE];
        // Upstream `secp256k1::SecretKey` doesn't `Deref` to bytes the
        // way the Parity fork did. The canonical way to extract the
        // 32-byte serialisation is via the `Display` / `as_ref` impls
        // (or `[u8; 32]::from(secret_key)` in newer versions). `[..]`
        // indexing through `SecretKey`'s `AsRef<[u8]>` works on v0.20.
        a.copy_from_slice(&key[..]);
        a.into()
    }
}

impl Deref for Secret {
    type Target = H256;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
