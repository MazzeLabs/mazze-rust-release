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

use tiny_keccak::{Hasher, Keccak};

pub trait Keccak256<T> {
    fn keccak256(&self) -> T
    where
        T: Sized;
}

/// Blanket impl for anything that exposes a byte slice — matches the
/// behaviour of `parity_crypto::Keccak256` so callers (e.g.
/// `Public::keccak256()` where `Public: H512`) keep working without
/// per-type impls.
impl<T: AsRef<[u8]> + ?Sized> Keccak256<[u8; 32]> for T {
    fn keccak256(&self) -> [u8; 32] {
        // tiny-keccak v2: constructor moved from `new_keccak256` to
        // `v256`, and `Hasher::update`/`finalize` are now trait methods.
        let mut keccak = Keccak::v256();
        let mut result = [0u8; 32];
        keccak.update(self.as_ref());
        keccak.finalize(&mut result);
        result
    }
}
