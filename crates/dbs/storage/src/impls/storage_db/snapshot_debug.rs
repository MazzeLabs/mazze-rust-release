// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

pub trait MptValueKind: Debug {
    fn value_eq(&self, maybe_value: Option<&[u8]>) -> bool;
}

impl MptValueKind for () {
    fn value_eq(&self, maybe_value: Option<&[u8]>) -> bool {
        maybe_value.is_none()
    }
}

impl MptValueKind for Box<[u8]> {
    fn value_eq(&self, maybe_value: Option<&[u8]>) -> bool {
        maybe_value.map_or(false, |v| v.eq(&**self))
    }
}

pub fn check_key_value_load<Db, Value: MptValueKind>(
    snapshot_db: &Db,
    mut kv_iter: impl FallibleIterator<Item = (Vec<u8>, Value), Error = Error>,
    check_value: bool,
) -> Result<u64>
where
    for<'db> Db: OpenSnapshotMptTrait<'db>,
    for<'db> <Db as OpenSnapshotMptTrait<'db>>::SnapshotDbBorrowSharedType:
        SnapshotMptTraitRead,
{
    let mut checker_count = 0;
    let mut mpt = snapshot_db.open_snapshot_mpt_shared()?;

    let mut cursor = MptCursor::<
        &mut dyn SnapshotMptTraitRead,
        BasicPathNode<&mut dyn SnapshotMptTraitRead>,
    >::new(&mut mpt);
    cursor.load_root()?;
    while let Some((access_key, expected_value)) = kv_iter.next()? {
        let terminal =
            cursor.open_path_for_key::<access_mode::Read>(&access_key)?;
        if check_value {
            let mpt_value = match terminal {
                CursorOpenPathTerminal::Arrived => {
                    cursor.current_node_mut().value_as_slice().into_option()
                }
                CursorOpenPathTerminal::ChildNotFound { .. } => None,
                CursorOpenPathTerminal::PathDiverted(_) => None,
            };
            if !expected_value.value_eq(mpt_value) {
                error!(
                    "mpt value doesn't match snapshot kv. Expected {:?}, got {:?}",
                    expected_value, mpt_value,
                );
            }
        }
        checker_count += 1;
    }
    cursor.finish()?;

    Ok(checker_count)
}

use crate::{
    impls::{
        errors::*,
        merkle_patricia_trie::{
            mpt_cursor::{BasicPathNode, CursorOpenPathTerminal, MptCursor},
            TrieNodeTrait,
        },
    },
    storage_db::{snapshot_db::OpenSnapshotMptTrait, SnapshotMptTraitRead},
    utils::access_mode,
};
use fallible_iterator::FallibleIterator;
use std::fmt::Debug;
