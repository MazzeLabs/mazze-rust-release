// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

/// Methods that a trie node type should implement in general.
/// Note that merkle hash isn't necessarily stored together with
/// a trie node because the merkle hash is mainly used when
/// obtaining a proof or computed when committing a block.
/// Merkle hash maybe stored in different way for IO optimizations.
pub trait TrieNodeTrait: Default {
    type NodeRefType: NodeRefTrait;
    type ChildrenTableType;

    fn compressed_path_ref(&self) -> CompressedPathRef;

    fn has_value(&self) -> bool;

    fn get_children_count(&self) -> u8;

    fn value_as_slice(&self) -> MptValue<&[u8]>;

    fn set_compressed_path(&mut self, compressed_path: CompressedPathRaw);

    /// Unsafe because it's assumed that the child_index is valid but the child
    /// doesn't exist.
    unsafe fn add_new_child_unchecked<T>(&mut self, child_index: u8, child: T)
    where
        ChildrenTableItem<Self::NodeRefType>:
            WrappedCreateFrom<T, Self::NodeRefType>;

    /// Unsafe because it's assumed that the child_index already exists.
    unsafe fn get_child_mut_unchecked(
        &mut self, child_index: u8,
    ) -> &mut Self::NodeRefType;

    /// Unsafe because it's assumed that the child_index already exists.
    unsafe fn replace_child_unchecked<T>(&mut self, child_index: u8, child: T)
    where
        ChildrenTableItem<Self::NodeRefType>:
            WrappedCreateFrom<T, Self::NodeRefType>;

    /// Unsafe because it's assumed that the child_index already exists.
    unsafe fn delete_child_unchecked(&mut self, child_index: u8);

    /// Delete value when we know that it already exists.
    unsafe fn delete_value_unchecked(&mut self) -> Box<[u8]>;

    fn replace_value_valid(
        &mut self, valid_value: Box<[u8]>,
    ) -> MptValue<Box<[u8]>>;

    fn get_children_table_ref(&self) -> &Self::ChildrenTableType;

    fn compute_merkle(
        &self, children_merkles: MaybeMerkleTableRef,
        path_without_first_nibble: bool,
    ) -> MerkleHash {
        compute_merkle(
            self.compressed_path_ref(),
            path_without_first_nibble,
            children_merkles,
            self.value_as_slice().into_option(),
        )
    }
}

// This trie node isn't memory efficient.
#[derive(Clone, Debug, PartialEq)]
pub struct VanillaTrieNode<NodeRefT: NodeRefTrait> {
    compressed_path: CompressedPathRaw,
    mpt_value: MptValue<Box<[u8]>>,
    children_table: VanillaChildrenTable<NodeRefT>,
    merkle_hash: MerkleHash,
}

impl<NodeRefT: 'static + NodeRefTrait> Default for VanillaTrieNode<NodeRefT>
where
    ChildrenTableItem<NodeRefT>: DefaultChildrenItem<NodeRefT>,
{
    fn default() -> Self {
        Self {
            compressed_path: Default::default(),
            mpt_value: MptValue::None,
            children_table: Default::default(),
            merkle_hash: MERKLE_NULL_NODE,
        }
    }
}

impl<'node, NodeRefT: 'static + NodeRefTrait> GetChildTrait<'node>
    for VanillaTrieNode<NodeRefT>
where
    ChildrenTableItem<NodeRefT>: DefaultChildrenItem<NodeRefT>,
{
    type ChildIdType = &'node NodeRefT;

    fn get_child(&'node self, child_index: u8) -> Option<&'node NodeRefT> {
        self.children_table.get_child(child_index)
    }
}

impl<'node, NodeRefT: 'static + NodeRefTrait> TrieNodeWalkTrait<'node>
    for VanillaTrieNode<NodeRefT>
where
    ChildrenTableItem<NodeRefT>: DefaultChildrenItem<NodeRefT>,
{
}

impl<NodeRefT: 'static + NodeRefTrait> TrieNodeTrait
    for VanillaTrieNode<NodeRefT>
where
    ChildrenTableItem<NodeRefT>: DefaultChildrenItem<NodeRefT>,
{
    type ChildrenTableType = VanillaChildrenTable<NodeRefT>;
    type NodeRefType = NodeRefT;

    fn compressed_path_ref(&self) -> CompressedPathRef {
        self.compressed_path.as_ref()
    }

    fn has_value(&self) -> bool {
        self.mpt_value.is_some()
    }

    fn get_children_count(&self) -> u8 {
        self.children_table.get_children_count()
    }

    fn value_as_slice(&self) -> MptValue<&[u8]> {
        match &self.mpt_value {
            MptValue::None => MptValue::None,
            MptValue::TombStone => MptValue::TombStone,
            MptValue::Some(v) => MptValue::Some(v.as_ref()),
        }
    }

    fn set_compressed_path(&mut self, compressed_path: CompressedPathRaw) {
        self.compressed_path = compressed_path;
    }

    unsafe fn add_new_child_unchecked<T>(&mut self, child_index: u8, child: T)
    where
        ChildrenTableItem<NodeRefT>: WrappedCreateFrom<T, NodeRefT>,
    {
        ChildrenTableItem::<NodeRefT>::take_from(
            self.children_table.get_child_mut_unchecked(child_index),
            child,
        );
        *self.children_table.get_children_count_mut() += 1;
    }

    unsafe fn get_child_mut_unchecked(
        &mut self, child_index: u8,
    ) -> &mut NodeRefT {
        self.children_table.get_child_mut_unchecked(child_index)
    }

    unsafe fn replace_child_unchecked<T>(&mut self, child_index: u8, child: T)
    where
        ChildrenTableItem<NodeRefT>: WrappedCreateFrom<T, NodeRefT>,
    {
        ChildrenTableItem::<NodeRefT>::take_from(
            self.children_table.get_child_mut_unchecked(child_index),
            child,
        );
    }

    unsafe fn delete_child_unchecked(&mut self, child_index: u8) {
        ChildrenTableItem::<NodeRefT>::take_from(
            self.children_table.get_child_mut_unchecked(child_index),
            ChildrenTableItem::<NodeRefT>::no_child(),
        );
        *self.children_table.get_children_count_mut() -= 1;
    }

    unsafe fn delete_value_unchecked(&mut self) -> Box<[u8]> {
        self.mpt_value.take().unwrap()
    }

    fn replace_value_valid(
        &mut self, valid_value: Box<[u8]>,
    ) -> MptValue<Box<[u8]>> {
        let new_mpt_value = if valid_value.len() == 0 {
            MptValue::TombStone
        } else {
            MptValue::Some(valid_value)
        };

        std::mem::replace(&mut self.mpt_value, new_mpt_value)
    }

    fn get_children_table_ref(&self) -> &VanillaChildrenTable<NodeRefT> {
        &self.children_table
    }
}

impl<NodeRefT: NodeRefTrait> VanillaTrieNode<NodeRefT> {
    pub fn new(
        merkle: MerkleHash, children_table: VanillaChildrenTable<NodeRefT>,
        maybe_value: Option<Box<[u8]>>, compressed_path: CompressedPathRaw,
    ) -> Self {
        let mpt_value = match maybe_value {
            None => MptValue::None,
            Some(v) => {
                if v.len() == 0 {
                    MptValue::TombStone
                } else {
                    MptValue::Some(v)
                }
            }
        };

        Self {
            compressed_path,
            mpt_value,
            children_table,
            merkle_hash: merkle.clone(),
        }
    }

    pub fn get_merkle(&self) -> &MerkleHash {
        &self.merkle_hash
    }

    pub fn set_merkle(&mut self, merkle: &MerkleHash) {
        self.merkle_hash = merkle.clone();
    }
}

impl VanillaTrieNode<MerkleHash> {
    pub fn get_children_merkles(&self) -> MaybeMerkleTableRef {
        if self.get_children_count() > 0 {
            Some(&self.children_table.get_children_table())
        } else {
            None
        }
    }

    pub fn get_merkle_hash_wo_compressed_path(&self) -> MerkleHash {
        compute_node_merkle(
            self.get_children_merkles(),
            self.value_as_slice().into_option(),
        )
    }
}

impl<NodeRefT: 'static + NodeRefTrait> Encodable for VanillaTrieNode<NodeRefT>
where
    ChildrenTableItem<NodeRefT>: DefaultChildrenItem<NodeRefT>,
{
    fn rlp_append(&self, s: &mut RlpStream) {
        s.begin_unbounded_list()
            .append(self.get_merkle())
            .append(self.get_children_table_ref())
            .append(&self.value_as_slice().into_option());

        let compressed_path_ref = self.compressed_path_ref();
        if compressed_path_ref.path_size() > 0 {
            s.append(&compressed_path_ref);
        }

        s.finalize_unbounded_list();
    }
}

impl<NodeRefT: 'static + NodeRefTrait> Decodable for VanillaTrieNode<NodeRefT>
where
    ChildrenTableItem<NodeRefT>: DefaultChildrenItem<NodeRefT>,
{
    fn decode(rlp: &Rlp) -> ::std::result::Result<Self, DecoderError> {
        let compressed_path;
        if rlp.item_count()? != 4 {
            compressed_path = CompressedPathRaw::new(&[], 0);
        } else {
            compressed_path = rlp.val_at(3)?;
        }

        Ok(VanillaTrieNode::new(
            MerkleHash::from_slice(rlp.val_at::<Vec<u8>>(0)?.as_slice()),
            rlp.val_at::<VanillaChildrenTable<NodeRefT>>(1)?,
            rlp.val_at::<Option<Vec<u8>>>(2)?
                .map(|v| v.into_boxed_slice()),
            compressed_path,
        ))
    }
}

impl<NodeRefT: 'static + NodeRefTrait + Serialize> Serialize
    for VanillaTrieNode<NodeRefT>
where
    ChildrenTableItem<NodeRefT>: DefaultChildrenItem<NodeRefT>,
{
    fn serialize<S>(
        &self, serializer: S,
    ) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("VanillaTrieNode", 4)?;
        state.serialize_field("compressedPath", &self.compressed_path)?;

        let val = match &self.mpt_value {
            MptValue::None => None,
            MptValue::TombStone => Some("".to_owned()),
            MptValue::Some(v) => {
                Some("0x".to_owned() + v.as_ref().to_hex::<String>().as_ref())
            }
        };

        state.serialize_field("mptValue", &val)?;
        state
            .serialize_field("childrenTable", self.get_children_table_ref())?;
        state.serialize_field("merkleHash", self.get_merkle())?;
        state.end()
    }
}

impl<'a, NodeRefT: 'static + NodeRefTrait + Deserialize<'a>> Deserialize<'a>
    for VanillaTrieNode<NodeRefT>
where
    ChildrenTableItem<NodeRefT>: DefaultChildrenItem<NodeRefT>,
{
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        deserializer.deserialize_struct(
            "VanillaTrieNode",
            FIELDS,
            VanillaTrieNodeVisitor {
                marker: PhantomData,
            },
        )
    }
}

const FIELDS: &'static [&'static str] =
    &["compressedPath", "mptValue", "childrenTable", "merkleHash"];

enum Field {
    CompressedPath,
    MptValue,
    ChildrenTable,
    MerkleHash,
}

impl<'de> Deserialize<'de> for Field {
    fn deserialize<D>(deserializer: D) -> Result<Field, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct FieldVisitor;

        impl<'de> Visitor<'de> for FieldVisitor {
            type Value = Field;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("`compressedPath` or `mptValue` or `childrenTable` or `merkleHash`")
            }

            fn visit_str<E>(self, value: &str) -> Result<Field, E>
            where
                E: de::Error,
            {
                match value {
                    "compressedPath" => Ok(Field::CompressedPath),
                    "mptValue" => Ok(Field::MptValue),
                    "childrenTable" => Ok(Field::ChildrenTable),
                    "merkleHash" => Ok(Field::MerkleHash),
                    _ => Err(de::Error::unknown_field(value, FIELDS)),
                }
            }
        }

        deserializer.deserialize_identifier(FieldVisitor)
    }
}

struct VanillaTrieNodeVisitor<NodeRefT> {
    marker: PhantomData<NodeRefT>,
}

impl<'de, NodeRefT: 'static + NodeRefTrait + Deserialize<'de>> Visitor<'de>
    for VanillaTrieNodeVisitor<NodeRefT>
where
    ChildrenTableItem<NodeRefT>: DefaultChildrenItem<NodeRefT>,
{
    type Value = VanillaTrieNode<NodeRefT>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("struct VanillaTrieNode")
    }

    fn visit_map<V>(self, mut map: V) -> Result<Self::Value, V::Error>
    where
        V: MapAccess<'de>,
    {
        let mut compressed_path = None;
        let mut mpt_value = None;
        let mut children_table = None;
        let mut merkle_hash = None;
        while let Some(key) = map.next_key()? {
            match key {
                Field::CompressedPath => {
                    if compressed_path.is_some() {
                        return Err(de::Error::duplicate_field(
                            "compressedPath",
                        ));
                    }
                    compressed_path = Some(map.next_value()?);
                }
                Field::MptValue => {
                    if mpt_value.is_some() {
                        return Err(de::Error::duplicate_field("mptValue"));
                    }
                    mpt_value = Some(map.next_value()?);
                }
                Field::ChildrenTable => {
                    if children_table.is_some() {
                        return Err(de::Error::duplicate_field(
                            "childrenTable",
                        ));
                    }
                    children_table = Some(map.next_value()?);
                }
                Field::MerkleHash => {
                    if merkle_hash.is_some() {
                        return Err(de::Error::duplicate_field("merkleHash"));
                    }
                    merkle_hash = Some(map.next_value()?);
                }
            }
        }

        let compressed_path = compressed_path
            .ok_or_else(|| de::Error::missing_field("compressedPath"))?;
        let mpt_value: Option<String> =
            mpt_value.ok_or_else(|| de::Error::missing_field("mptValue"))?;
        let mpt_value: Option<Vec<u8>> = match mpt_value {
            Some(v) => {
                if v.is_empty() {
                    Some(vec![])
                } else if let (Some(s), true) =
                    (v.strip_prefix("0x"), v.len() & 1 == 0)
                {
                    Some(FromHex::from_hex(s).map_err(|e| {
                        de::Error::custom(format!(
                            "mptValue: invalid hex: {}",
                            e
                        ))
                    })?)
                } else {
                    return Err(de::Error::custom("mptValue: invalid format. Expected a 0x-prefixed hex string with even length"));
                }
            }
            _ => None,
        };

        let children_table = children_table
            .ok_or_else(|| de::Error::missing_field("childrenTable"))?;
        let merkle_hash = merkle_hash
            .ok_or_else(|| de::Error::missing_field("merkleHash"))?;

        Ok(VanillaTrieNode::new(
            merkle_hash,
            children_table,
            mpt_value.map(|v| v.into_boxed_slice()),
            compressed_path,
        ))
    }
}

use super::{
    super::super::utils::WrappedCreateFrom,
    children_table::*,
    compressed_path::*,
    merkle::{compute_merkle, compute_node_merkle, MaybeMerkleTableRef},
    walk::*,
};
use primitives::{MerkleHash, MptValue, MERKLE_NULL_NODE};
use rlp::*;
use rustc_hex::{FromHex, ToHex};
use serde::{
    de::{self, MapAccess, Visitor},
    ser::SerializeStruct,
    Deserialize, Deserializer, Serialize, Serializer,
};
use std::{fmt, marker::PhantomData, vec::Vec};
