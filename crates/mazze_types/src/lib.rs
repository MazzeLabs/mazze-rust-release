// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

extern crate ethereum_types;
extern crate rlp;
extern crate rlp_derive;
extern crate serde;
extern crate serde_derive;

use std::ops::{Add, Index, IndexMut};

pub use ethereum_types::{
    Address, BigEndianHash, Bloom, BloomInput, Public, Secret, Signature, H128,
    H160, H256, H512, H520, H64, U128, U256, U512, U64,
};
use rlp::{Decodable, DecoderError, Encodable, Rlp, RlpStream};
use rlp_derive::{RlpDecodable, RlpEncodable};
use serde::ser::SerializeMap;
use serde_derive::{Deserialize, Serialize};

pub use self::space_util::AddressSpaceUtil;

#[derive(
    Eq,
    PartialEq,
    Hash,
    Copy,
    Clone,
    Debug,
    Ord,
    PartialOrd,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Space {
    Native,
    #[serde(rename(serialize = "evm", deserialize = "evm"))]
    Ethereum,
}

impl From<Space> for String {
    fn from(space: Space) -> Self {
        let str: &'static str = space.into();
        str.into()
    }
}

impl From<Space> for &'static str {
    fn from(space: Space) -> Self {
        match space {
            Space::Native => "native",
            Space::Ethereum => "evm",
        }
    }
}

impl Encodable for Space {
    fn rlp_append(&self, s: &mut RlpStream) {
        let type_int: u8 = match self {
            Space::Native => 1,
            Space::Ethereum => 2,
        };
        type_int.rlp_append(s)
    }
}

impl Decodable for Space {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        match u8::decode(rlp)? {
            1u8 => Ok(Space::Native),
            2u8 => Ok(Space::Ethereum),
            _ => Err(DecoderError::Custom("Unrecognized space byte.")),
        }
    }
}

#[derive(
    Default, Copy, Clone, Debug, Eq, PartialEq, RlpEncodable, RlpDecodable,
)]
pub struct AllChainID {
    native: u32,
    ethereum: u32,
}

impl AllChainID {
    pub fn new(native: u32, ethereum: u32) -> Self {
        Self { native, ethereum }
    }

    pub fn fake_for_virtual(chain_id: u32) -> Self {
        Self {
            native: chain_id,
            ethereum: chain_id,
        }
    }

    pub fn in_space(&self, space: Space) -> u32 {
        match space {
            Space::Native => self.native,
            Space::Ethereum => self.ethereum,
        }
    }

    pub fn in_native_space(&self) -> u32 {
        self.in_space(Space::Native)
    }

    pub fn in_evm_space(&self) -> u32 {
        self.in_space(Space::Ethereum)
    }
}

impl Default for Space {
    fn default() -> Self {
        Space::Native
    }
}

#[derive(Default, Eq, PartialEq, Hash, Copy, Clone, Debug, Ord, PartialOrd)]
pub struct AddressWithSpace {
    pub address: Address,
    pub space: Space,
}

impl AddressWithSpace {
    #[inline]
    pub fn assert_native(&self) {
        assert_eq!(self.space, Space::Native)
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct SpaceMap<T> {
    native: T,
    evm: T,
}

impl<T> SpaceMap<T> {
    pub const fn new(native: T, evm: T) -> Self {
        SpaceMap { native, evm }
    }

    #[inline]
    pub fn in_space(&self, space: Space) -> &T {
        match space {
            Space::Native => &self.native,
            Space::Ethereum => &self.evm,
        }
    }

    #[inline]
    pub fn in_space_mut(&mut self, space: Space) -> &mut T {
        match space {
            Space::Native => &mut self.native,
            Space::Ethereum => &mut self.evm,
        }
    }

    pub fn zip3<B, C>(
        a: SpaceMap<T>, b: SpaceMap<B>, c: SpaceMap<C>,
    ) -> SpaceMap<(T, B, C)> {
        SpaceMap {
            native: (a.native, b.native, c.native),
            evm: (a.evm, b.evm, c.evm),
        }
    }

    pub fn zip4<B, C, D>(
        a: SpaceMap<T>, b: SpaceMap<B>, c: SpaceMap<C>, d: SpaceMap<D>,
    ) -> SpaceMap<(T, B, C, D)> {
        SpaceMap {
            native: (a.native, b.native, c.native, d.native),
            evm: (a.evm, b.evm, c.evm, d.evm),
        }
    }

    pub fn map_sum<F: FnMut(&T) -> U, U: Add<U, Output = U>>(
        &self, mut f: F,
    ) -> U {
        f(&self.native) + f(&self.evm)
    }

    pub const fn size(&self) -> usize {
        2
    }

    pub fn map_all<U, F: Fn(T) -> U>(self, f: F) -> SpaceMap<U> {
        SpaceMap {
            native: f(self.native),
            evm: f(self.evm),
        }
    }

    pub fn apply_all<U, F: FnMut(&mut T) -> U>(
        &mut self, mut f: F,
    ) -> SpaceMap<U> {
        SpaceMap {
            native: f(&mut self.native),
            evm: f(&mut self.evm),
        }
    }
}

impl<T> Index<Space> for SpaceMap<T> {
    type Output = T;

    fn index(&self, space: Space) -> &Self::Output {
        self.in_space(space)
    }
}

impl<T> IndexMut<Space> for SpaceMap<T> {
    fn index_mut(&mut self, space: Space) -> &mut Self::Output {
        self.in_space_mut(space)
    }
}

impl<T: serde::Serialize> serde::Serialize for SpaceMap<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.size()))?;
        map.serialize_entry("core", &self.native)?;
        map.serialize_entry("espace", &self.evm)?;
        map.end()
    }
}

pub mod space_util {
    use super::{Address, AddressWithSpace, Space};

    pub trait AddressSpaceUtil: Sized {
        fn with_space(self, space: Space) -> AddressWithSpace;
        fn with_native_space(self) -> AddressWithSpace {
            self.with_space(Space::Native)
        }
        fn with_evm_space(self) -> AddressWithSpace {
            self.with_space(Space::Ethereum)
        }
    }

    impl AddressSpaceUtil for Address {
        fn with_space(self, space: Space) -> AddressWithSpace {
            AddressWithSpace {
                address: self,
                space,
            }
        }
    }
}

pub fn maybe_address(address: &Address) -> Option<Address> {
    if address.is_zero() {
        None
    } else {
        Some(*address)
    }
}

/// The KECCAK hash of an empty bloom filter (0x00 * 256)
pub const KECCAK_EMPTY_BLOOM: H256 = H256([
    0xd3, 0x97, 0xb3, 0xb0, 0x43, 0xd8, 0x7f, 0xcd, 0x6f, 0xad, 0x12, 0x91,
    0xff, 0x0b, 0xfd, 0x16, 0x40, 0x1c, 0x27, 0x48, 0x96, 0xd8, 0xc6, 0x3a,
    0x92, 0x37, 0x27, 0xf0, 0x77, 0xb8, 0xe0, 0xb5,
]);

pub fn hexstr_to_h256(hex_str: &str) -> H256 {
    assert_eq!(hex_str.len(), 64);
    let mut bytes: [u8; 32] = Default::default();

    for i in 0..32 {
        bytes[i] = u8::from_str_radix(&hex_str[i * 2..i * 2 + 2], 16).unwrap();
    }

    H256::from(bytes)
}

pub mod address_util {
    use super::Address;

    pub const TYPE_BITS_BUILTIN: u8 = 0x00;
    pub const TYPE_BITS_CONTRACT: u8 = 0x80;
    pub const TYPE_BITS_USER_ACCOUNT: u8 = 0x10;

    pub trait AddressUtil: Sized + Ord {
        fn type_byte(&self) -> &u8;

        fn type_byte_mut(&mut self) -> &mut u8;

        fn is_null_address(&self) -> bool;

        #[inline]
        fn address_type_bits(&self) -> u8 {
            self.type_byte() & 0xf0
        }

        #[inline]
        fn set_address_type_bits(&mut self, type_bits: u8) {
            let type_byte = self.type_byte_mut();
            *type_byte &= 0x0f;
            *type_byte |= type_bits;
        }

        #[cfg(feature = "storage_benchmark_no_account_space_check")]
        #[inline]
        fn is_genesis_valid_address(&self) -> bool {
            true
        }

        #[cfg(not(feature = "storage_benchmark_no_account_space_check"))]
        #[inline]
        fn is_genesis_valid_address(&self) -> bool {
            self.is_contract_address()
                || self.is_user_account_address()
                || self.is_builtin_address()
                || self.is_null_address()
        }

        #[inline]
        fn is_contract_address(&self) -> bool {
            self.address_type_bits() == TYPE_BITS_CONTRACT
        }

        #[inline]
        fn is_user_account_address(&self) -> bool {
            self.address_type_bits() == TYPE_BITS_USER_ACCOUNT
        }

        #[inline]
        fn is_builtin_address(&self) -> bool {
            self.address_type_bits() == TYPE_BITS_BUILTIN
                && !self.is_null_address()
        }

        #[inline]
        fn set_contract_type_bits(&mut self) {
            self.set_address_type_bits(TYPE_BITS_CONTRACT);
        }

        #[inline]
        fn set_user_account_type_bits(&mut self) {
            self.set_address_type_bits(TYPE_BITS_USER_ACCOUNT);
        }
    }

    impl AddressUtil for Address {
        #[inline]
        fn type_byte(&self) -> &u8 {
            &self.as_fixed_bytes()[0]
        }

        #[inline]
        fn type_byte_mut(&mut self) -> &mut u8 {
            &mut self.as_fixed_bytes_mut()[0]
        }

        #[inline]
        fn is_null_address(&self) -> bool {
            self.is_zero()
        }
    }

    impl AddressUtil for &[u8] {
        #[inline]
        fn type_byte(&self) -> &u8 {
            &self[0]
        }

        #[inline]
        fn type_byte_mut(&mut self) -> &mut u8 {
            unreachable!()
        }

        #[inline]
        fn is_null_address(&self) -> bool {
            self.iter().all(|&byte| byte == 0u8)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{Address, AddressUtil};

        #[test]
        fn test_set_type_bits() {
            let mut address = Address::default();

            address.set_contract_type_bits();
            assert!(address.is_contract_address());
            assert!(!address.is_user_account_address());

            address.set_user_account_type_bits();
            assert!(address.is_user_account_address());

            for types in 0..16 {
                let type_bits = types << 4;
                address.set_address_type_bits(type_bits);
                assert_eq!(address.address_type_bits(), type_bits);
            }
        }
    }
}
