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

use super::{Generator, KeyPair, SECP256K1};
use rand::rngs::OsRng;
use rand::RngCore;
use secp256k1::{PublicKey, SecretKey};

/// Randomly generates new keypair, instantiating the RNG each time.
pub struct Random;

impl Generator for Random {
    type Error = ::std::io::Error;

    fn generate(&mut self) -> Result<KeyPair, Self::Error> {
        match OsRng.generate() {
            Ok(pair) => Ok(pair),
            Err(void) => match void {}, // LLVM unreachable
        }
    }
}

impl Generator for OsRng {
    type Error = crate::Void;

    fn generate(&mut self) -> Result<KeyPair, Self::Error> {
        // Draw 32 random bytes from OsRng and loop until they form a
        // valid secp256k1 scalar. Retry probability ≈ 2⁻¹²⁸.
        let sec = loop {
            let mut bytes = [0u8; 32];
            self.fill_bytes(&mut bytes);
            if let Ok(sec) = SecretKey::from_slice(&bytes) {
                break sec;
            }
            // Probability of failure ≈ 2^-128; mainly defensive.
        };
        let publ = PublicKey::from_secret_key(&SECP256K1, &sec);

        Ok(KeyPair::from_keypair(sec, publ))
    }
}
