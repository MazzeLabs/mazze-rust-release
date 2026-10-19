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

use crate::Public;
use mazze_types::{BigEndianHash as _, H256, U256};
use secp256k1::{constants::CURVE_ORDER, PublicKey};

/// Whether the public key is valid (encodes a point on the secp256k1
/// curve). With the upstream `secp256k1` crate, this is enforced at
/// construction time by `PublicKey::from_slice` — so this helper now
/// just round-trips through that constructor.
pub fn public_is_valid(public: &Public) -> bool {
    let mut data = [4u8; 65];
    data[1..65].copy_from_slice(&public[0..64]);
    PublicKey::from_slice(&data).is_ok()
}

/// Return secp256k1 elliptic curve order. Used by BIP32 derivation
/// in `extended.rs`.
pub fn curve_order() -> U256 {
    H256::from_slice(&CURVE_ORDER).into_uint()
}
