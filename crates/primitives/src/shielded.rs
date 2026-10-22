// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! The shielded transaction body (spec §6.2).
//!
//! Shielded transactions are carried in their own block section, apart from
//! the account transactions: they have no sender, nonce, signature or gas.
//! The proof is not part of the body. It travels as a witness sidecar, and
//! the body commits to it through `proof_hash`.
//!
//! The encoding is canonical: a body has exactly one valid encoding, and the
//! decoder rejects anything else (trailing bytes, wrong lengths, field
//! elements out of range). Every byte is paid for (lesson Z-4).

use crate::hash::keccak;
use mazze_types::{Address, Space, H256, H512};
use tiny_keccak::{Hasher, Shake};
use rlp::{Decodable, DecoderError, Encodable, Rlp, RlpStream};

/// The only body version.
pub const SHIELDED_TX_VERSION: u8 = 1;

/// The KoalaBear prime, `2^31 - 2^24 + 1`.
pub const KOALABEAR_P: u32 = 0x7f00_0001;

/// Base-field elements in an in-circuit digest (spec §2).
pub const DIGEST_ELEMENTS: usize = 12;

/// Bytes of an encoded digest: each element as 4 bytes little-endian.
pub const DIGEST_BYTES: usize = DIGEST_ELEMENTS * 4;

/// Domain of `proof_hash`. Fixed length, so domain and proof never overlap.
pub const PROOF_HASH_DOMAIN: &[u8] = b"mazze/shielded/proof-hash/v1";

/// The commitment to a proof carried in the witness sidecar: 512 bits of
/// SHAKE256, in line with the spec's >= 372-bit digest rule.
pub fn proof_hash(proof: &[u8]) -> H512 {
    let mut shake = Shake::v256();
    shake.update(PROOF_HASH_DOMAIN);
    shake.update(proof);
    let mut out = H512::zero();
    shake.finalize(out.as_bytes_mut());
    out
}

/// Largest accepted proof in bytes, for every proof system (spec §6.2). The
/// prover crate checks that its own cap equals this one.
pub const MAX_PROOF_BYTES: usize = 400 * 1024;

pub const RECOVERY_TAG_BYTES: usize = 32;
pub const RECOVERY_SLOT_BYTES: usize = 64;
pub const RECOVERY_MAC_BYTES: usize = 16;

/// A Poseidon2 digest over KoalaBear, kept in its canonical byte form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ShieldedDigest([u8; DIGEST_BYTES]);

impl ShieldedDigest {
    /// Accepts only canonical encodings: every element below the prime.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecoderError> {
        let bytes: [u8; DIGEST_BYTES] = bytes
            .try_into()
            .map_err(|_| DecoderError::Custom("Bad shielded digest length"))?;
        for limb in bytes.chunks_exact(4) {
            let element = u32::from_le_bytes(limb.try_into().unwrap());
            if element >= KOALABEAR_P {
                return Err(DecoderError::Custom(
                    "Non-canonical shielded digest element",
                ));
            }
        }
        Ok(ShieldedDigest(bytes))
    }

    pub fn from_elements(elements: [u32; DIGEST_ELEMENTS]) -> Option<Self> {
        let mut bytes = [0u8; DIGEST_BYTES];
        for (limb, element) in bytes.chunks_exact_mut(4).zip(elements) {
            if element >= KOALABEAR_P {
                return None;
            }
            limb.copy_from_slice(&element.to_le_bytes());
        }
        Some(ShieldedDigest(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; DIGEST_BYTES] { &self.0 }
}

impl Encodable for ShieldedDigest {
    fn rlp_append(&self, s: &mut RlpStream) {
        // `encode_value`, not `append`: the caller's `append` counts the item.
        s.encoder().encode_value(&self.0);
    }
}

impl Decodable for ShieldedDigest {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        if !rlp.is_data() {
            return Err(DecoderError::RlpExpectedToBeData);
        }
        Self::from_bytes(rlp.data()?)
    }
}

/// The sender's recovery slot (spec §9.5). Always present; an unused slot is
/// random bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoverySlot {
    pub tag: [u8; RECOVERY_TAG_BYTES],
    pub slot: [u8; RECOVERY_SLOT_BYTES],
    pub mac: [u8; RECOVERY_MAC_BYTES],
}

impl Encodable for RecoverySlot {
    fn rlp_append(&self, s: &mut RlpStream) {
        s.begin_list(3);
        s.append(&&self.tag[..]);
        s.append(&&self.slot[..]);
        s.append(&&self.mac[..]);
    }
}

fn fixed_bytes<const N: usize>(
    rlp: &Rlp, index: usize,
) -> Result<[u8; N], DecoderError> {
    let item = rlp.at(index)?;
    if !item.is_data() {
        return Err(DecoderError::RlpExpectedToBeData);
    }
    item.data()?
        .try_into()
        .map_err(|_| DecoderError::Custom("Bad shielded field length"))
}

impl Decodable for RecoverySlot {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        if rlp.item_count()? != 3 {
            return Err(DecoderError::RlpIncorrectListLen);
        }
        Ok(RecoverySlot {
            tag: fixed_bytes(rlp, 0)?,
            slot: fixed_bytes(rlp, 1)?,
            mac: fixed_bytes(rlp, 2)?,
        })
    }
}

/// Where a shield-out pays to. The shielded space itself is not a valid
/// destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShieldedDest {
    pub space: Space,
    pub address: Address,
}

/// A shielded transaction body, version 1 (spec §6.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShieldedTxBody {
    /// What is proven: the relation and its state.
    pub pool_version: u8,
    /// How it is proven.
    pub proof_system_id: u8,
    pub anchor: ShieldedDigest,
    pub nf: [ShieldedDigest; 2],
    pub cm_out: [ShieldedDigest; 2],
    pub recovery: RecoverySlot,
    /// Non-zero only for a shield-in.
    pub v_public_in: u64,
    /// Non-zero only for a shield-out.
    pub v_public_out: u64,
    /// Present iff `v_public_out > 0`.
    pub dest: Option<ShieldedDest>,
    pub fee: u64,
    /// `proof_hash(proof)` of the proof carried in the witness sidecar.
    pub proof_hash: H512,
}

impl ShieldedTxBody {
    /// Decodes a standalone body, rejecting trailing bytes. Use this, not
    /// `rlp::decode`, for bodies received on their own.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, DecoderError> {
        let rlp = Rlp::new(bytes);
        if rlp.payload_info()?.total() != bytes.len() {
            return Err(DecoderError::RlpIsTooBig);
        }
        rlp.as_val()
    }

    /// The transaction id. It covers `proof_hash`, so it commits to the proof.
    pub fn hash(&self) -> H256 { keccak(rlp::encode(self)) }

    /// Rules that need no chain state. The decoder enforces them too, so a
    /// decoded body always satisfies them.
    pub fn check_structure(&self) -> Result<(), &'static str> {
        if self.nf[0] == self.nf[1] {
            return Err("duplicate nullifier");
        }
        match self.dest {
            None if self.v_public_out > 0 => {
                Err("shield-out without a destination")
            }
            Some(_) if self.v_public_out == 0 => {
                Err("destination without a shield-out")
            }
            Some(ShieldedDest {
                space: Space::Shielded,
                ..
            }) => Err("the shielded space is not a destination"),
            _ => Ok(()),
        }
    }
}

impl ShieldedTxBody {
    /// A structurally valid body, distinct per `seed`, for tests in any
    /// crate. It carries no valid proof.
    #[doc(hidden)]
    pub fn sample_for_tests(seed: u32) -> Self {
        let digest = |i: u32| {
            ShieldedDigest::from_elements(std::array::from_fn(|j| {
                (seed.wrapping_mul(0x9e37_79b9) ^ (i * 16 + j as u32))
                    % KOALABEAR_P
            }))
            .expect("reduced below the prime")
        };
        ShieldedTxBody {
            pool_version: 1,
            proof_system_id: 1,
            anchor: digest(1),
            nf: [digest(2), digest(3)],
            cm_out: [digest(4), digest(5)],
            recovery: RecoverySlot {
                tag: [seed as u8; RECOVERY_TAG_BYTES],
                slot: [7; RECOVERY_SLOT_BYTES],
                mac: [9; RECOVERY_MAC_BYTES],
            },
            v_public_in: 0,
            v_public_out: 0,
            dest: None,
            fee: 1_000,
            proof_hash: H512::from_low_u64_be(seed as u64),
        }
    }
}

/// Domain of the sighash.
pub const SIGHASH_DOMAIN: &[u8] = b"mazze/shielded/sighash/v1";

impl ShieldedTxBody {
    /// The digest the proof binds as a public input (spec §6.3): SHAKE256
    /// over the domain, the chain id and every body field except
    /// `proof_hash`, which cannot be covered because the proof depends on
    /// the sighash. The body itself commits to the proof through
    /// `proof_hash`.
    pub fn sighash(&self, chain_id: u32) -> [u8; 32] {
        let mut fields = RlpStream::new_list(13);
        self.append_signed_fields(&mut fields);
        let mut shake = Shake::v256();
        shake.update(SIGHASH_DOMAIN);
        shake.update(&chain_id.to_le_bytes());
        shake.update(&fields.out());
        let mut out = [0u8; 32];
        shake.finalize(&mut out);
        out
    }

    /// The first 13 items of the encoding: everything but `proof_hash`.
    fn append_signed_fields(&self, s: &mut RlpStream) {
        s.append(&SHIELDED_TX_VERSION);
        s.append(&self.pool_version);
        s.append(&self.proof_system_id);
        s.append(&self.anchor);
        s.append(&self.nf[0]);
        s.append(&self.nf[1]);
        s.append(&self.cm_out[0]);
        s.append(&self.cm_out[1]);
        s.append(&self.recovery);
        s.append(&self.v_public_in);
        s.append(&self.v_public_out);
        match &self.dest {
            None => {
                s.begin_list(0);
            }
            Some(dest) => {
                s.begin_list(2);
                s.append(&dest.space);
                s.append(&dest.address);
            }
        }
        s.append(&self.fee);
    }
}

impl Encodable for ShieldedTxBody {
    fn rlp_append(&self, s: &mut RlpStream) {
        s.begin_list(14);
        self.append_signed_fields(s);
        s.append(&self.proof_hash);
    }
}

impl Decodable for ShieldedTxBody {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        if rlp.item_count()? != 14 {
            return Err(DecoderError::RlpIncorrectListLen);
        }
        if rlp.val_at::<u8>(0)? != SHIELDED_TX_VERSION {
            return Err(DecoderError::Custom(
                "Unknown shielded transaction version",
            ));
        }
        let dest_rlp = rlp.at(11)?;
        let dest = match dest_rlp.item_count()? {
            0 if dest_rlp.is_list() => None,
            2 => Some(ShieldedDest {
                space: dest_rlp.val_at(0)?,
                address: dest_rlp.val_at(1)?,
            }),
            _ => return Err(DecoderError::RlpIncorrectListLen),
        };
        let body = ShieldedTxBody {
            pool_version: rlp.val_at(1)?,
            proof_system_id: rlp.val_at(2)?,
            anchor: rlp.val_at(3)?,
            nf: [rlp.val_at(4)?, rlp.val_at(5)?],
            cm_out: [rlp.val_at(6)?, rlp.val_at(7)?],
            recovery: rlp.val_at(8)?,
            v_public_in: rlp.val_at(9)?,
            v_public_out: rlp.val_at(10)?,
            dest,
            fee: rlp.val_at(12)?,
            proof_hash: rlp.val_at(13)?,
        };
        body.check_structure().map_err(DecoderError::Custom)?;
        // One body, one encoding: anything the encoder would not produce
        // (a list where data belongs, a non-minimal length) is refused, so
        // the bytes relayed are always the bytes hashed.
        if rlp.as_raw() != &rlp::encode(&body)[..] {
            return Err(DecoderError::Custom("Non-canonical shielded body"));
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_canonical_encoding_decodes() {
        let body = ShieldedTxBody::sample_for_tests(7);
        let bytes = rlp::encode(&body);
        assert_eq!(ShieldedTxBody::decode_canonical(&bytes).unwrap(), body);
        // The anchor (item 3) re-encoded as a one-item list of its bytes.
        let rlp = Rlp::new(&bytes);
        let mut s = RlpStream::new_list(14);
        for i in 0..14 {
            if i == 3 {
                s.begin_list(1);
                s.append(&&body.anchor.as_bytes()[..]);
            } else {
                s.append_raw(rlp.at(i).unwrap().as_raw(), 1);
            }
        }
        assert!(ShieldedTxBody::decode_canonical(&s.out()).is_err());
    }

    fn digest(seed: u32) -> ShieldedDigest {
        ShieldedDigest::from_elements(std::array::from_fn(|i| {
            (seed.wrapping_mul(2654435761).wrapping_add(i as u32))
                % KOALABEAR_P
        }))
        .unwrap()
    }

    fn body() -> ShieldedTxBody {
        ShieldedTxBody {
            pool_version: 1,
            proof_system_id: 1,
            anchor: digest(1),
            nf: [digest(2), digest(3)],
            cm_out: [digest(4), digest(5)],
            recovery: RecoverySlot {
                tag: [7; RECOVERY_TAG_BYTES],
                slot: [8; RECOVERY_SLOT_BYTES],
                mac: [9; RECOVERY_MAC_BYTES],
            },
            v_public_in: 0,
            v_public_out: 0,
            dest: None,
            fee: 1_000,
            proof_hash: H512::repeat_byte(0xab),
        }
    }

    fn shield_out() -> ShieldedTxBody {
        ShieldedTxBody {
            v_public_out: 25,
            dest: Some(ShieldedDest {
                space: Space::Ethereum,
                address: Address::repeat_byte(0x11),
            }),
            ..body()
        }
    }

    /// Re-encodes the body with one item replaced by raw RLP.
    fn with_item(body: &ShieldedTxBody, index: usize, raw: &[u8]) -> Vec<u8> {
        let encoded = rlp::encode(body);
        let rlp = Rlp::new(&encoded);
        let mut s = RlpStream::new_list(14);
        for i in 0..14 {
            if i == index {
                s.append_raw(raw, 1);
            } else {
                s.append_raw(rlp.at(i).unwrap().as_raw(), 1);
            }
        }
        s.out().to_vec()
    }

    #[test]
    fn round_trips() {
        for body in [body(), shield_out()] {
            let encoded = rlp::encode(&body);
            let decoded = ShieldedTxBody::decode_canonical(&encoded).unwrap();
            assert_eq!(decoded, body);
            assert_eq!(rlp::encode(&decoded), encoded);
        }
    }

    #[test]
    fn encoding_is_one_self_contained_item() {
        for body in [body(), shield_out()] {
            let encoded = rlp::encode(&body);
            let rlp = Rlp::new(&encoded);
            assert_eq!(rlp.payload_info().unwrap().total(), encoded.len());
            assert_eq!(rlp.item_count().unwrap(), 14);

            // Nested, the body stays a single item.
            let mut s = RlpStream::new_list(2);
            s.append(&body).append(&body);
            let list = s.out();
            let rlp = Rlp::new(&list);
            assert_eq!(rlp.payload_info().unwrap().total(), list.len());
            assert_eq!(rlp.item_count().unwrap(), 2);
            assert_eq!(rlp.val_at::<ShieldedTxBody>(0).unwrap(), body);
            assert_eq!(rlp.val_at::<ShieldedTxBody>(1).unwrap(), body);
        }
    }

    #[test]
    fn proof_hash_is_512_bit_shake256_with_domain() {
        let h = proof_hash(b"proof");
        assert_eq!(h.as_bytes().len(), 64);
        assert_ne!(h, proof_hash(b"proog"));
        assert_ne!(h, proof_hash(b""));
        // The domain is part of the input: plain SHAKE256 differs.
        let mut plain = [0u8; 64];
        let mut shake = Shake::v256();
        shake.update(b"proof");
        shake.finalize(&mut plain);
        assert_ne!(h.as_bytes(), &plain);
    }

    #[test]
    fn sighash_covers_every_field_but_the_proof_hash() {
        let base = body().sighash(1);
        assert_ne!(base, body().sighash(2), "chain id");
        let mut other = body();
        other.proof_hash = H512::repeat_byte(0x01);
        assert_eq!(other.sighash(1), base, "the proof hash is not covered");
        let mut changes: Vec<Box<dyn Fn(&mut ShieldedTxBody)>> = vec![
            Box::new(|b| b.pool_version += 1),
            Box::new(|b| b.proof_system_id += 1),
            Box::new(|b| b.anchor = digest(9)),
            Box::new(|b| b.nf[1] = digest(9)),
            Box::new(|b| b.cm_out[0] = digest(9)),
            Box::new(|b| b.recovery.tag[3] ^= 1),
            Box::new(|b| b.recovery.slot[0] ^= 1),
            Box::new(|b| b.recovery.mac[15] ^= 1),
            Box::new(|b| b.v_public_in += 1),
            Box::new(|b| b.fee += 1),
            Box::new(|b| {
                b.v_public_out = 1;
                b.dest = Some(ShieldedDest {
                    space: Space::Native,
                    address: Address::repeat_byte(2),
                })
            }),
        ];
        for (i, change) in changes.iter_mut().enumerate() {
            let mut b = body();
            change(&mut b);
            assert_ne!(b.sighash(1), base, "field change {i} not covered");
        }
        let mut native = shield_out();
        native.dest.as_mut().unwrap().space = Space::Native;
        assert_ne!(native.sighash(1), shield_out().sighash(1), "dest space");
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut encoded = rlp::encode(&body()).to_vec();
        encoded.push(0x80);
        assert!(ShieldedTxBody::decode_canonical(&encoded).is_err());
    }

    #[test]
    fn body_size_is_constant_for_a_given_shape() {
        // 5 digests, the recovery slot, the proof hash and small integers.
        let size = rlp::encode(&body()).len();
        // 3 version bytes, 5 digests of 1+48, the recovery slot (118),
        // v_public_in/out, an empty dest, the fee, a 2+64 proof hash, and the
        // 3-byte list header.
        assert_eq!(size, 441);
        let out = rlp::encode(&shield_out()).len();
        // The empty dest list (1 byte) becomes a list header, the space byte
        // and the 21-byte address.
        assert_eq!(out - size, 1 + 1 + 21 - 1);
    }

    #[test]
    fn hash_commits_to_every_field() {
        let base = body().hash();
        let mut changed = body();
        changed.proof_hash = H512::repeat_byte(0xac);
        assert_ne!(changed.hash(), base);
        let mut changed = body();
        changed.recovery.mac[0] ^= 1;
        assert_ne!(changed.hash(), base);
        let mut changed = body();
        changed.fee += 1;
        assert_ne!(changed.hash(), base);
    }

    #[test]
    fn rejects_out_of_range_digest_elements() {
        let mut raw = *digest(1).as_bytes();
        raw[..4].copy_from_slice(&KOALABEAR_P.to_le_bytes());
        assert!(ShieldedDigest::from_bytes(&raw).is_err());
        raw[..4].copy_from_slice(&(KOALABEAR_P - 1).to_le_bytes());
        assert!(ShieldedDigest::from_bytes(&raw).is_ok());

        raw[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        let item = rlp::encode(&&raw[..]);
        assert!(ShieldedTxBody::decode_canonical(&with_item(&body(), 3, &item))
            .is_err());
    }

    #[test]
    fn rejects_wrong_lengths() {
        let short = rlp::encode(&&digest(1).as_bytes()[..DIGEST_BYTES - 1]);
        assert!(ShieldedTxBody::decode_canonical(&with_item(&body(), 4, &short))
            .is_err());
        let short_hash = rlp::encode(&&[0u8; 32][..]);
        assert!(ShieldedTxBody::decode_canonical(&with_item(
            &body(),
            13,
            &short_hash
        ))
        .is_err());
    }

    #[test]
    fn rejects_trailing_and_missing_items() {
        let encoded = rlp::encode(&body());
        let rlp = Rlp::new(&encoded);

        let mut s = RlpStream::new_list(15);
        for i in 0..14 {
            s.append_raw(rlp.at(i).unwrap().as_raw(), 1);
        }
        s.append(&0u8);
        assert!(ShieldedTxBody::decode_canonical(&s.out()).is_err());

        let mut s = RlpStream::new_list(13);
        for i in 0..13 {
            s.append_raw(rlp.at(i).unwrap().as_raw(), 1);
        }
        assert!(ShieldedTxBody::decode_canonical(&s.out()).is_err());
    }

    #[test]
    fn rejects_non_minimal_integers() {
        // 1000 with a leading zero byte.
        let item = rlp::encode(&&[0u8, 0x03, 0xe8][..]);
        assert!(ShieldedTxBody::decode_canonical(&with_item(&body(), 12, &item))
            .is_err());
    }

    #[test]
    fn rejects_unknown_version() {
        let item = rlp::encode(&2u8);
        assert!(ShieldedTxBody::decode_canonical(&with_item(&body(), 0, &item))
            .is_err());
    }

    #[test]
    fn structural_rules() {
        let mut dup = body();
        dup.nf[1] = dup.nf[0];
        assert!(dup.check_structure().is_err());
        assert!(ShieldedTxBody::decode_canonical(&rlp::encode(&dup)).is_err());

        let mut no_dest = shield_out();
        no_dest.dest = None;
        assert!(no_dest.check_structure().is_err());

        let mut stray_dest = shield_out();
        stray_dest.v_public_out = 0;
        assert!(stray_dest.check_structure().is_err());

        let mut into_pool = shield_out();
        into_pool.dest.as_mut().unwrap().space = Space::Shielded;
        assert!(into_pool.check_structure().is_err());
        assert!(ShieldedTxBody::decode_canonical(&rlp::encode(&into_pool))
            .is_err());

        assert!(body().check_structure().is_ok());
        assert!(shield_out().check_structure().is_ok());
    }
}
