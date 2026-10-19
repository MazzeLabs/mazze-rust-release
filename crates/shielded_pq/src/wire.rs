//! Consensus wire format for proofs (spec §6.2): fixed-width and canonical.
//!
//! A minimal, non-self-describing serde format, written here rather than
//! taken from a dependency because it parses untrusted bytes on the
//! consensus path:
//!
//! - integers are fixed-width little-endian (`usize` as 8 bytes);
//! - sequence, string and byte lengths are 8 bytes;
//! - `bool` is one byte, 0 or 1; an `Option` tag is one byte, 0 or 1;
//! - enum variants are a 4-byte index;
//! - tuples, arrays and structs are their fields in order.
//!
//! Plonky3 writes field elements as 4 bytes and rejects values >= p, so a
//! proof has exactly one encoding. The decoder rejects trailing bytes and any
//! length that exceeds the bytes left, so a forged length cannot make it
//! allocate more than the input size, which the caller caps.

use serde::{
    de::{self, DeserializeSeed, IntoDeserializer, SeqAccess, VariantAccess},
    ser, Deserialize, Serialize,
};
use std::fmt;

use crate::ps1::Proof;

#[derive(Debug, PartialEq, Eq)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str(&self.0) }
}
impl std::error::Error for Error {}
impl ser::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self { Error(msg.to_string()) }
}
impl de::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self { Error(msg.to_string()) }
}

fn err<T>(msg: &str) -> Result<T, Error> { Err(Error(msg.into())) }

/// Encodes any serializable value in the wire format.
pub fn to_bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    value.serialize(&mut Writer { out: &mut out })?;
    Ok(out)
}

/// Decodes exactly `bytes`: trailing bytes are an error.
pub fn from_bytes<'de, T: Deserialize<'de>>(bytes: &'de [u8]) -> Result<T, Error> {
    let mut reader = Reader { input: bytes };
    let value = T::deserialize(&mut reader)?;
    if !reader.input.is_empty() {
        return err("trailing bytes");
    }
    Ok(value)
}

pub fn encode(proof: &Proof) -> Vec<u8> {
    to_bytes(proof).expect("an in-memory proof always encodes")
}

/// Decodes a proof of at most `max_len` bytes; a longer input is rejected
/// before it is parsed.
pub fn decode(bytes: &[u8], max_len: usize) -> Result<Proof, Error> {
    if bytes.len() > max_len {
        return Err(Error(format!(
            "proof is {} bytes, cap {max_len}",
            bytes.len()
        )));
    }
    from_bytes(bytes)
}

struct Writer<'a> {
    out: &'a mut Vec<u8>,
}

impl Writer<'_> {
    fn len(&mut self, len: Option<usize>) -> Result<(), Error> {
        match len {
            Some(len) => {
                self.out.extend_from_slice(&(len as u64).to_le_bytes());
                Ok(())
            }
            None => err("sequence without a known length"),
        }
    }
}

impl<'a, 'b> ser::Serializer for &'a mut Writer<'b> {
    type Error = Error;
    type Ok = ();
    type SerializeMap = Self;
    type SerializeSeq = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;

    fn is_human_readable(&self) -> bool { false }

    fn serialize_bool(self, v: bool) -> Result<(), Error> {
        self.out.push(v as u8);
        Ok(())
    }
    fn serialize_i8(self, v: i8) -> Result<(), Error> {
        self.out.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_i16(self, v: i16) -> Result<(), Error> {
        self.out.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_i32(self, v: i32) -> Result<(), Error> {
        self.out.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_i64(self, v: i64) -> Result<(), Error> {
        self.out.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_u8(self, v: u8) -> Result<(), Error> {
        self.out.push(v);
        Ok(())
    }
    fn serialize_u16(self, v: u16) -> Result<(), Error> {
        self.out.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_u32(self, v: u32) -> Result<(), Error> {
        self.out.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_u64(self, v: u64) -> Result<(), Error> {
        self.out.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_f32(self, _: f32) -> Result<(), Error> { err("floats are not allowed") }
    fn serialize_f64(self, _: f64) -> Result<(), Error> { err("floats are not allowed") }
    fn serialize_char(self, _: char) -> Result<(), Error> { err("chars are not allowed") }
    fn serialize_str(self, v: &str) -> Result<(), Error> { self.serialize_bytes(v.as_bytes()) }
    fn serialize_bytes(self, v: &[u8]) -> Result<(), Error> {
        self.len(Some(v.len()))?;
        self.out.extend_from_slice(v);
        Ok(())
    }
    fn serialize_none(self) -> Result<(), Error> {
        self.out.push(0);
        Ok(())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), Error> {
        self.out.push(1);
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), Error> { Ok(()) }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), Error> { Ok(()) }
    fn serialize_unit_variant(
        self, _: &'static str, index: u32, _: &'static str,
    ) -> Result<(), Error> {
        self.serialize_u32(index)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self, _: &'static str, value: &T,
    ) -> Result<(), Error> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self, _: &'static str, index: u32, _: &'static str, value: &T,
    ) -> Result<(), Error> {
        self.serialize_u32(index)?;
        value.serialize(self)
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Self, Error> {
        self.len(len)?;
        Ok(self)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self, Error> { Ok(self) }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self, Error> { Ok(self) }
    fn serialize_tuple_variant(
        self, _: &'static str, index: u32, _: &'static str, _: usize,
    ) -> Result<Self, Error> {
        self.serialize_u32(index)?;
        Ok(self)
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Self, Error> {
        self.len(len)?;
        Ok(self)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self, Error> { Ok(self) }
    fn serialize_struct_variant(
        self, _: &'static str, index: u32, _: &'static str, _: usize,
    ) -> Result<Self, Error> {
        self.serialize_u32(index)?;
        Ok(self)
    }
}

macro_rules! compound {
    ($trait:ident, $method:ident $(, $key:ident)?) => {
        impl<'a, 'b> ser::$trait for &'a mut Writer<'b> {
            type Error = Error;
            type Ok = ();
            fn $method<T: Serialize + ?Sized>(
                &mut self, $($key: &'static str,)? value: &T,
            ) -> Result<(), Error> {
                $(let _ = $key;)?
                value.serialize(&mut **self)
            }
            fn end(self) -> Result<(), Error> { Ok(()) }
        }
    };
}
compound!(SerializeSeq, serialize_element);
compound!(SerializeTuple, serialize_element);
compound!(SerializeTupleStruct, serialize_field);
compound!(SerializeTupleVariant, serialize_field);
compound!(SerializeStruct, serialize_field, key);
compound!(SerializeStructVariant, serialize_field, key);

impl<'a, 'b> ser::SerializeMap for &'a mut Writer<'b> {
    type Error = Error;
    type Ok = ();
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
        key.serialize(&mut **self)
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Error> { Ok(()) }
}

struct Reader<'de> {
    input: &'de [u8],
}

impl<'de> Reader<'de> {
    fn take(&mut self, n: usize) -> Result<&'de [u8], Error> {
        if n > self.input.len() {
            return err("unexpected end of input");
        }
        let (head, tail) = self.input.split_at(n);
        self.input = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        Ok(self.take(N)?.try_into().expect("took N bytes"))
    }

    fn u64(&mut self) -> Result<u64, Error> { Ok(u64::from_le_bytes(self.array()?)) }

    /// A length prefix. Every element takes at least one byte, so a length
    /// beyond the bytes left is malformed and rejected before allocation.
    fn len(&mut self) -> Result<usize, Error> {
        let len = self.u64()?;
        if len > self.input.len() as u64 {
            return err("length exceeds the input");
        }
        Ok(len as usize)
    }

    fn tag(&mut self) -> Result<bool, Error> {
        match self.array::<1>()?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => err("a boolean or option tag is not 0 or 1"),
        }
    }
}

impl<'de, 'a> de::Deserializer<'de> for &'a mut Reader<'de> {
    type Error = Error;

    fn is_human_readable(&self) -> bool { false }

    fn deserialize_any<V: de::Visitor<'de>>(self, _: V) -> Result<V::Value, Error> {
        err("the wire format is not self-describing")
    }
    fn deserialize_bool<V: de::Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_bool(self.tag()?)
    }
    fn deserialize_i8<V: de::Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_i8(i8::from_le_bytes(self.array()?))
    }
    fn deserialize_i16<V: de::Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_i16(i16::from_le_bytes(self.array()?))
    }
    fn deserialize_i32<V: de::Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_i32(i32::from_le_bytes(self.array()?))
    }
    fn deserialize_i64<V: de::Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_i64(i64::from_le_bytes(self.array()?))
    }
    fn deserialize_u8<V: de::Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_u8(self.array::<1>()?[0])
    }
    fn deserialize_u16<V: de::Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_u16(u16::from_le_bytes(self.array()?))
    }
    fn deserialize_u32<V: de::Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_u32(u32::from_le_bytes(self.array()?))
    }
    fn deserialize_u64<V: de::Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_u64(self.u64()?)
    }
    fn deserialize_f32<V: de::Visitor<'de>>(self, _: V) -> Result<V::Value, Error> {
        err("floats are not allowed")
    }
    fn deserialize_f64<V: de::Visitor<'de>>(self, _: V) -> Result<V::Value, Error> {
        err("floats are not allowed")
    }
    fn deserialize_char<V: de::Visitor<'de>>(self, _: V) -> Result<V::Value, Error> {
        err("chars are not allowed")
    }
    fn deserialize_str<V: de::Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        let len = self.len()?;
        let bytes = self.take(len)?;
        v.visit_borrowed_str(std::str::from_utf8(bytes).map_err(|e| Error(e.to_string()))?)
    }
    fn deserialize_string<V: de::Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_str(v)
    }
    fn deserialize_bytes<V: de::Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        let len = self.len()?;
        v.visit_borrowed_bytes(self.take(len)?)
    }
    fn deserialize_byte_buf<V: de::Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        self.deserialize_bytes(v)
    }
    fn deserialize_option<V: de::Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        if self.tag()? {
            v.visit_some(self)
        } else {
            v.visit_none()
        }
    }
    fn deserialize_unit<V: de::Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        v.visit_unit()
    }
    fn deserialize_unit_struct<V: de::Visitor<'de>>(
        self, _: &'static str, v: V,
    ) -> Result<V::Value, Error> {
        v.visit_unit()
    }
    fn deserialize_newtype_struct<V: de::Visitor<'de>>(
        self, _: &'static str, v: V,
    ) -> Result<V::Value, Error> {
        v.visit_newtype_struct(self)
    }
    fn deserialize_seq<V: de::Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        let len = self.len()?;
        v.visit_seq(Counted { reader: self, left: len })
    }
    fn deserialize_tuple<V: de::Visitor<'de>>(self, len: usize, v: V) -> Result<V::Value, Error> {
        v.visit_seq(Counted { reader: self, left: len })
    }
    fn deserialize_tuple_struct<V: de::Visitor<'de>>(
        self, _: &'static str, len: usize, v: V,
    ) -> Result<V::Value, Error> {
        v.visit_seq(Counted { reader: self, left: len })
    }
    fn deserialize_map<V: de::Visitor<'de>>(self, v: V) -> Result<V::Value, Error> {
        let len = self.len()?;
        v.visit_map(Counted { reader: self, left: len })
    }
    fn deserialize_struct<V: de::Visitor<'de>>(
        self, _: &'static str, fields: &'static [&'static str], v: V,
    ) -> Result<V::Value, Error> {
        v.visit_seq(Counted { reader: self, left: fields.len() })
    }
    fn deserialize_enum<V: de::Visitor<'de>>(
        self, _: &'static str, _: &'static [&'static str], v: V,
    ) -> Result<V::Value, Error> {
        v.visit_enum(self)
    }
    fn deserialize_identifier<V: de::Visitor<'de>>(self, _: V) -> Result<V::Value, Error> {
        err("identifiers are not encoded")
    }
    fn deserialize_ignored_any<V: de::Visitor<'de>>(self, _: V) -> Result<V::Value, Error> {
        err("the wire format is not self-describing")
    }
}

struct Counted<'a, 'de> {
    reader: &'a mut Reader<'de>,
    left: usize,
}

impl<'de> SeqAccess<'de> for Counted<'_, 'de> {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self, seed: T,
    ) -> Result<Option<T::Value>, Error> {
        if self.left == 0 {
            return Ok(None);
        }
        self.left -= 1;
        seed.deserialize(&mut *self.reader).map(Some)
    }
    fn size_hint(&self) -> Option<usize> { Some(self.left) }
}

impl<'de> de::MapAccess<'de> for Counted<'_, 'de> {
    type Error = Error;
    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self, seed: K,
    ) -> Result<Option<K::Value>, Error> {
        if self.left == 0 {
            return Ok(None);
        }
        self.left -= 1;
        seed.deserialize(&mut *self.reader).map(Some)
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        seed.deserialize(&mut *self.reader)
    }
    fn size_hint(&self) -> Option<usize> { Some(self.left) }
}

impl<'de, 'a> de::EnumAccess<'de> for &'a mut Reader<'de> {
    type Error = Error;
    type Variant = Self;
    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self), Error> {
        let index = u32::from_le_bytes(self.array()?);
        let value = seed.deserialize(index.into_deserializer())?;
        Ok((value, self))
    }
}

impl<'de, 'a> VariantAccess<'de> for &'a mut Reader<'de> {
    type Error = Error;
    fn unit_variant(self) -> Result<(), Error> { Ok(()) }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        seed.deserialize(self)
    }
    fn tuple_variant<V: de::Visitor<'de>>(self, len: usize, v: V) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_tuple(self, len, v)
    }
    fn struct_variant<V: de::Visitor<'de>>(
        self, fields: &'static [&'static str], v: V,
    ) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_struct(self, "", fields, v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    enum E {
        A,
        B(u16),
        C { x: u8 },
    }

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct S {
        a: u8,
        b: u32,
        c: u64,
        d: bool,
        e: Option<u16>,
        f: Vec<[u64; 4]>,
        g: (u8, u32),
        h: Vec<E>,
    }

    fn sample() -> S {
        S {
            a: 1,
            b: 0x0203_0405,
            c: u64::MAX,
            d: true,
            e: Some(7),
            f: vec![[1, 2, 3, 4]],
            g: (9, 10),
            h: vec![E::A, E::B(3), E::C { x: 4 }],
        }
    }

    #[test]
    fn round_trips_with_fixed_widths() {
        let bytes = to_bytes(&sample()).unwrap();
        // 1 + 4 + 8 + 1 + (1 + 2) + (8 + 32) + (1 + 4) + (8 + 4 + 6 + 5)
        assert_eq!(bytes.len(), 85);
        assert_eq!(from_bytes::<S>(&bytes).unwrap(), sample());
    }

    #[test]
    fn rejects_malformed_input() {
        let bytes = to_bytes(&sample()).unwrap();
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(from_bytes::<S>(&longer).is_err(), "trailing byte");
        assert!(from_bytes::<S>(&bytes[..bytes.len() - 1]).is_err(), "truncated");

        let mut bad_bool = bytes.clone();
        bad_bool[13] = 2;
        assert!(from_bytes::<S>(&bad_bool).is_err(), "bool 2");
        let mut bad_tag = bytes.clone();
        bad_tag[14] = 2;
        assert!(from_bytes::<S>(&bad_tag).is_err(), "option tag 2");

        // A huge length prefix is rejected without allocating.
        let mut huge = bytes.clone();
        huge[17..25].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(from_bytes::<S>(&huge).is_err(), "huge length");

        let mut bad_variant = bytes.clone();
        let at = bytes.len() - 5;
        bad_variant[at..at + 4].copy_from_slice(&9u32.to_le_bytes());
        assert!(from_bytes::<S>(&bad_variant).is_err(), "unknown variant");
    }
}
