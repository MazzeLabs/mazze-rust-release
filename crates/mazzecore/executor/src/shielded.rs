// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use ark_bls12_381::Fr;
use ark_crypto_primitives::sponge::poseidon::traits::find_poseidon_ark_and_mds;
use ark_crypto_primitives::sponge::poseidon::{PoseidonConfig, PoseidonSponge};
use ark_crypto_primitives::sponge::CryptographicSponge;
use ark_ff::{BigInteger, PrimeField};
use mazze_types::{H256, U256};
use once_cell::sync::OnceCell;

const POSEIDON_RATE: usize = 2;
const POSEIDON_CAPACITY: usize = 1;
const POSEIDON_ALPHA: u64 = 17;
const POSEIDON_FULL_ROUNDS: usize = 8;
const POSEIDON_PARTIAL_ROUNDS: usize = 31;
const POSEIDON_SKIP_MATRICES: u64 = 0;

static POSEIDON_PARAMS: OnceCell<PoseidonConfig<Fr>> = OnceCell::new();

pub fn poseidon_config() -> &'static PoseidonConfig<Fr> {
    POSEIDON_PARAMS.get_or_init(|| {
        let (ark, mds) = find_poseidon_ark_and_mds::<Fr>(
            Fr::MODULUS_BIT_SIZE as u64,
            POSEIDON_RATE,
            POSEIDON_FULL_ROUNDS as u64,
            POSEIDON_PARTIAL_ROUNDS as u64,
            POSEIDON_SKIP_MATRICES,
        );
        PoseidonConfig::new(
            POSEIDON_FULL_ROUNDS,
            POSEIDON_PARTIAL_ROUNDS,
            POSEIDON_ALPHA,
            mds,
            ark,
            POSEIDON_RATE,
            POSEIDON_CAPACITY,
        )
    })
}

pub fn poseidon_hash(inputs: &[Fr]) -> Fr {
    let mut sponge = PoseidonSponge::new(poseidon_config());
    sponge.absorb(&inputs);
    sponge.squeeze_field_elements(1)[0]
}

pub fn poseidon_hash2(left: &Fr, right: &Fr) -> Fr {
    let mut sponge = PoseidonSponge::new(poseidon_config());
    sponge.absorb(left);
    sponge.absorb(right);
    sponge.squeeze_field_elements(1)[0]
}

pub fn fr_from_h256(value: &H256) -> Fr {
    Fr::from_be_bytes_mod_order(value.as_ref())
}

pub fn fr_from_bytes(bytes: &[u8]) -> Fr {
    Fr::from_be_bytes_mod_order(bytes)
}

pub fn fr_from_u256(value: &U256) -> Fr {
    let mut bytes = [0u8; 32];
    value.to_big_endian(&mut bytes);
    Fr::from_be_bytes_mod_order(&bytes)
}

pub fn fr_to_h256(value: &Fr) -> H256 {
    let bigint = value.into_bigint();
    let mut bytes = [0u8; 32];
    let raw = bigint.to_bytes_be();
    let start = bytes.len().saturating_sub(raw.len());
    bytes[start..].copy_from_slice(&raw);
    H256::from(bytes)
}

pub fn split_recipient(recipient: &[u8; 64]) -> (Fr, Fr) {
    let mut left = [0u8; 32];
    let mut right = [0u8; 32];
    left.copy_from_slice(&recipient[..32]);
    right.copy_from_slice(&recipient[32..]);
    (fr_from_bytes(&left), fr_from_bytes(&right))
}

pub mod circuit;
