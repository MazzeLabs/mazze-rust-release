// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

#[cfg(test)]
mod slicer;
#[cfg(test)]
mod verifier;

#[derive(Clone, Default)]
pub struct FakeSnapshotMptDb {
    db: BTreeMap<Vec<u8>, SnapshotMptNode>,
    in_place_mode: bool,
    already_written: HashSet<Vec<u8>>,
    discard_write: bool,
}

impl FakeSnapshotMptDb {
    /// With discard_write, it tests if the MptMerger does not rely on the value
    /// to write to work correctly. In other words, MptMerger works correctly
    /// only reading the data to merge.
    pub fn new_discard_write() -> Self {
        Self {
            discard_write: true,
            ..Default::default()
        }
    }

    #[cfg(test)]
    fn reset(&mut self, in_place_mode: bool) {
        self.in_place_mode = in_place_mode;
        self.already_written.clear();
    }

    #[cfg(test)]
    fn assert_eq(&self, expected: &Self) {
        // FIXME: there is a known issue in save-as mode where there are nodes
        //  missing in the result db.
        // assert_eq!(self.db.len(), expected.db.len());

        // Check subtree size.
        for (k, node) in &expected.db {
            let maybe_got_node = self.db.get(k);
            match maybe_got_node {
                None => panic!("key {:?} not found in resulting mpt.", k),
                Some(got_node) => {
                    for (child_index, child_ref) in
                        node.get_children_table_ref().iter()
                    {
                        match got_node
                            .get_children_table_ref()
                            .get_child(child_index)
                        {
                            None => {
                                panic!(
                                    "Child {} not found. Expected\n\t{:?}\n\
                                    got\n\t{:?}\nkey {:?}",
                                    child_index, node, got_node, k,
                                )
                            }
                            Some(got_child_ref) => {
                                assert_eq!(
                                    child_ref.subtree_size, got_child_ref.subtree_size,
                                    "Subtree size of child {} mismatch. Expected\n\t{:?}\n\
                                    got\n\t{:?}\nkey {:?}",
                                    child_index, node, got_node, k,
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

struct FakeSnapshotMptDbIter<'a>(
    btree_map::Range<'a, Vec<u8>, SnapshotMptNode>,
);

impl SnapshotMptTraitRead for FakeSnapshotMptDb {
    fn get_merkle_root(&self) -> MerkleHash {
        unimplemented!()
    }

    fn load_node(
        &mut self, path: &dyn CompressedPathTrait,
    ) -> Result<Option<SnapshotMptNode>> {
        let result = Ok(self.db.get(&mpt_node_path_to_db_key(path)).cloned());
        if CHECK_LOADED_SNAPSHOT_MPT_NODE {
            if let Ok(Some(node)) = &result {
                node.is_valid(path);
            }
        }
        result
    }
}

impl SnapshotMptTraitReadAndIterate for FakeSnapshotMptDb {
    fn iterate_subtree_trie_nodes_without_root(
        &mut self, path: &dyn CompressedPathTrait,
    ) -> Result<Box<dyn SnapshotMptIteraterTrait + '_>> {
        let begin_key_excl = mpt_node_path_to_db_key(path);

        let mut end_key_excl = begin_key_excl.clone();
        // The key is non empty. See also comment for compressed_path_to_db_key.
        *end_key_excl.last_mut().unwrap() += 1;

        Ok(Box::new(FakeSnapshotMptDbIter(self.db.range((
            Excluded(begin_key_excl),
            Excluded(end_key_excl),
        )))))
    }
}

impl SnapshotMptTraitRw for FakeSnapshotMptDb {
    fn delete_node(&mut self, path: &dyn CompressedPathTrait) -> Result<()> {
        if self.discard_write {
            return Ok(());
        }
        let key = mpt_node_path_to_db_key(path);
        let old_value = self.db.remove(&key);
        if self.in_place_mode {
            assert!(
                old_value.is_some(),
                "Shouldn't delete node {:?} in in-place mode",
                key
            );
        } else {
            panic!("Shouldn't call delete_node in save-as mode. key {:?}", key);
        }
        if self.already_written.contains(&key) {
            panic!("Shouldn't delete a newly written node. key {:?}", key);
        }
        Ok(())
    }

    fn write_node(
        &mut self, path: &dyn CompressedPathTrait, trie_node: &SnapshotMptNode,
    ) -> Result<()> {
        if self.discard_write {
            return Ok(());
        }
        let key = mpt_node_path_to_db_key(path);
        self.db.insert(key.clone(), trie_node.clone());
        if !self.already_written.insert(key.clone()) {
            panic!("Shouldn't write a node more than one time. key {:?}", key);
        }
        Ok(())
    }
}

impl FallibleIterator for FakeSnapshotMptDbIter<'_> {
    type Error = Error;
    type Item = (CompressedPathRaw, SnapshotMptNode);

    fn next(&mut self) -> Result<Option<Self::Item>> {
        match self.0.next() {
            Some((k, v)) => {
                Ok(Some((mpt_node_path_from_db_key(k)?, v.clone())))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
fn assert_snapshot_mpt_formation(mpt_kv_iter: &DumpedMptKvIterator) {
    let snapshot_mpt_nodes;
    let delta_mpt_root = {
        let state_manager = new_state_manager_for_unit_test();
        let mut state = state_manager.get_state_for_genesis_write();
        for (key, value) in &mpt_kv_iter.kv {
            state
                .set(
                    StorageKey::AccountKey(key).with_native_space(),
                    value.clone(),
                )
                .expect("Failed to insert key.");
        }

        let mut epoch_id = EpochId::default();
        epoch_id.as_bytes_mut()[0] = 1;
        let root = state.compute_state_root().unwrap().state_root;
        state.commit(epoch_id).unwrap();

        snapshot_mpt_nodes =
            state_manager.number_committed_nodes.load(Ordering::Relaxed);

        root.delta_root
    };

    println!(
        "Checking snapshot mpt formation of {} keys with expected \
         merkle_root {:?} and number of nodes {}",
        mpt_kv_iter.kv.len(),
        delta_mpt_root,
        snapshot_mpt_nodes
    );

    let mut empty_snapshot_mpt = FakeSnapshotMptDb::default();
    let mut new_snapshot_mpt = FakeSnapshotMptDb::default();

    // Save-as mode.
    new_snapshot_mpt.reset(/* in_place_mode */ true);
    let snapshot_merkle_root =
        MptMerger::new(Some(&mut empty_snapshot_mpt), &mut new_snapshot_mpt)
            .merge(&mpt_kv_iter)
            .unwrap();
    assert_eq!(delta_mpt_root, snapshot_merkle_root);
    assert_eq!(new_snapshot_mpt.db.len(), snapshot_mpt_nodes);

    empty_snapshot_mpt.reset(/* in_place_mode */ true);
    let snapshot_merkle_root = MptMerger::new(None, &mut empty_snapshot_mpt)
        .merge(&mpt_kv_iter)
        .unwrap();
    assert_eq!(delta_mpt_root, snapshot_merkle_root);
    assert_eq!(empty_snapshot_mpt.db.len(), snapshot_mpt_nodes);
}

#[cfg(test)]
#[test]
fn test_mpt_node_path_to_from_db_key() {
    // First, construct some special compressed path in a node.
    let mpt_kv = [
        (vec![0x00, 0x10, 0x00, 0x00], vec![0x00]),
        (vec![0x00, 0x01, 0x00, 0x00, 0x02], vec![0x00]),
        (vec![0x00, 0x01, 0x00, 0x00, 0x03], vec![0x00]),
        (vec![0x00, 0x01, 0x02, 0x00], vec![0x00]),
    ];
    // Compressed path 1: [_0]
    // Compressed path 2: [_00000]
    // Compressed path 3: [_10_]
    // Compressed path 4: [000_]
    // Compressed path 5: [00]

    let state_manager = new_state_manager_for_unit_test();
    let mut state = state_manager.get_state_for_genesis_write();
    for (key, value) in &mpt_kv {
        state
            .set(
                StorageKey::AccountKey(key).with_native_space(),
                value.clone().into_boxed_slice(),
            )
            .expect("Failed to insert key.");
    }
    let mut epoch_id = EpochId::default();
    epoch_id.as_bytes_mut()[0] = 1;
    state.compute_state_root().unwrap();
    let state_root_with_aux_info = state.commit(epoch_id).unwrap();

    let state = state_manager
        .get_state_no_commit_inner(
            StateIndex::new_for_readonly(&epoch_id, &state_root_with_aux_info),
            /* try_open = */ false,
            true,
        )
        .unwrap()
        .unwrap();

    // Second, use compressed path as mpt_node_path to test
    // mpt_node_path_to_db_key / mpt_node_path_from_db_key.
    for (key, value) in &mpt_kv {
        let (v, proof) = state
            .get_with_proof(StorageKey::AccountKey(key).with_native_space())
            .unwrap();
        assert_eq!(v, Some(value.clone().into_boxed_slice()));
        for node in proof.delta_proof.unwrap().get_proof_nodes() {
            let compressed_path = node.compressed_path_ref();
            // mpt_node_path_to_db_key only works for paths with the beginning
            // nibble.
            if CompressedPathRaw::second_nibble(compressed_path.path_mask())
                == CompressedPathRaw::NO_MISSING_NIBBLE
            {
                let db_key = mpt_node_path_to_db_key(&compressed_path);
                let loaded_compressed_path =
                    mpt_node_path_from_db_key(&db_key).unwrap();
                assert_eq!(
                    &compressed_path as &dyn CompressedPathTrait,
                    &loaded_compressed_path as &dyn CompressedPathTrait,
                );
            }
        }
    }
}

#[cfg(test)]
#[test]
fn test_merkle_root() {
    // Merkle root of empty db.
    assert_snapshot_mpt_formation(&DumpedMptKvIterator::default());

    // Merkle root of random set of keys.
    let mut rng = get_rng_for_test();
    for _i in 0..5 {
        let keys: Vec<Vec<u8>> = generate_keys(TEST_NUMBER_OF_KEYS)
            .iter()
            .filter(|_| rng.gen_bool(0.1))
            .cloned()
            .collect();
        let mpt_kv_iter = DumpedMptKvIterator {
            kv: keys.iter().map(|k| (k[..].into(), k[..].into())).collect(),
        };
        assert_snapshot_mpt_formation(&mpt_kv_iter);
    }
}

#[cfg(test)]
#[test]
fn test_delete_all() {
    let mut rng = get_rng_for_test();
    let keys: Vec<Vec<u8>> = generate_keys(TEST_NUMBER_OF_KEYS)
        .iter()
        .filter(|_| rng.gen_bool(0.5))
        .cloned()
        .collect();
    let mpt_kv_iter = DumpedMptKvIterator {
        kv: keys.iter().map(|k| (k[..].into(), k[..].into())).collect(),
    };

    let mut empty_snapshot_mpt = FakeSnapshotMptDb::default();
    let mut snapshot_mpt = FakeSnapshotMptDb::default();

    // Now snapshot_mpt contains key-values.
    snapshot_mpt.reset(/* in_place_mode */ false);
    MptMerger::new(Some(&mut empty_snapshot_mpt), &mut snapshot_mpt)
        .merge(&mpt_kv_iter)
        .unwrap();

    let mpt_deleter = DumpedMptKvIterator {
        kv: keys
            .iter()
            .map(|k| (k[..].into(), Default::default()))
            .collect(),
    };

    // Save-as mode.
    empty_snapshot_mpt.reset(/* in_place_mode */ false);
    let merkle_root =
        MptMerger::new(Some(&mut snapshot_mpt), &mut empty_snapshot_mpt)
            .merge(&mpt_deleter)
            .unwrap();
    assert_eq!(MERKLE_NULL_NODE, merkle_root);
    assert_eq!(0, empty_snapshot_mpt.db.len());

    // In-place mode
    snapshot_mpt.reset(/* in_place_mode */ true);
    let merkle_root = MptMerger::new(None, &mut snapshot_mpt)
        .merge(&mpt_deleter)
        .unwrap();
    assert_eq!(MERKLE_NULL_NODE, merkle_root);
    assert_eq!(0, snapshot_mpt.db.len());
}

#[cfg(test)]
#[test]
fn test_inserts_deletes_and_subtree_size() {
    let keys: Vec<Vec<u8>> = generate_keys(TEST_NUMBER_OF_KEYS);
    let set_size = TEST_NUMBER_OF_KEYS / 10;
    let (keys_unchanged, keys_overwritten, keys_delete, keys_new) = (
        &keys[0..set_size],
        &keys[set_size..set_size * 2],
        &keys[set_size * 2..set_size * 3],
        &keys[set_size * 3..set_size * 4],
    );

    // Case 1. Start with a snapshot mpt consisting of keys_delete,
    // Apply the change, then end up with a snapshot mpt consisting of keys_new.
    let mpt_kv_iter = DumpedMptKvIterator {
        kv: keys_delete
            .iter()
            .map(|k| (k[..].into(), k[..].into()))
            .collect(),
    };

    let mut in_place_mod_mpt = FakeSnapshotMptDb::default();
    in_place_mod_mpt.reset(/* in_place_mode = */ true);
    let init_merkle_root = MptMerger::new(None, &mut in_place_mod_mpt)
        .merge(&mpt_kv_iter)
        .unwrap();

    let mut in_place_mod_mpt_discard_write =
        FakeSnapshotMptDb::new_discard_write();
    in_place_mod_mpt_discard_write.reset(/* in_place_mode = */ true);
    let merkle_root = MptMerger::new(None, &mut in_place_mod_mpt_discard_write)
        .merge(&mpt_kv_iter)
        .unwrap();
    assert_eq!(init_merkle_root, merkle_root);

    let mpt_kv_iter = DumpedMptKvIterator {
        kv: keys_new
            .iter()
            .map(|k| (k[..].into(), k[..].into()))
            .collect(),
    };
    let mut new_snapshot_mpt = FakeSnapshotMptDb::default();
    new_snapshot_mpt.reset(/* in_place_mode = */ true);
    let supposed_merkle_root = MptMerger::new(None, &mut new_snapshot_mpt)
        .merge(&mpt_kv_iter)
        .unwrap();

    let delta_mpt_iter = DumpedMptKvIterator {
        kv: [
            keys_delete
                .iter()
                .map(|k| (Vec::<u8>::from(&k[..]), Box::<[u8]>::default()))
                .collect::<Vec<_>>(),
            keys_new
                .iter()
                .map(|k| (Vec::<u8>::from(&k[..]), Box::<[u8]>::from(&k[..])))
                .collect::<Vec<_>>(),
        ]
        .concat(),
    };
    let mut save_as_mode_mpt = FakeSnapshotMptDb::default();
    let new_merkle_root =
        MptMerger::new(Some(&mut in_place_mod_mpt), &mut save_as_mode_mpt)
            .merge(&delta_mpt_iter)
            .unwrap();
    assert_eq!(new_merkle_root, supposed_merkle_root);
    save_as_mode_mpt.assert_eq(&new_snapshot_mpt);

    in_place_mod_mpt.reset(/* in_place_mode */ true);
    let new_merkle_root = MptMerger::new(None, &mut in_place_mod_mpt)
        .merge(&delta_mpt_iter)
        .unwrap();
    assert_eq!(new_merkle_root, supposed_merkle_root);
    in_place_mod_mpt.assert_eq(&new_snapshot_mpt);

    // Case 2. Start with a snapshot mpt consisting of keys_unchanged,
    // keys_overwritten, keys_delete, Apply the change, then end up with a
    // snapshot mpt consisting of (keys_unchanged, keys_overwritten, keys_new).
    let mpt_kv_iter = DumpedMptKvIterator {
        kv: [
            keys_unchanged
                .iter()
                .map(|k| (Vec::<u8>::from(&k[..]), Box::<[u8]>::from(&k[..])))
                .collect::<Vec<_>>(),
            keys_delete
                .iter()
                .map(|k| (Vec::<u8>::from(&k[..]), Box::<[u8]>::from(&k[..])))
                .collect::<Vec<_>>(),
            keys_overwritten
                .iter()
                .map(|k| (Vec::<u8>::from(&k[..]), Box::<[u8]>::from(&k[0..2])))
                .collect::<Vec<_>>(),
        ]
        .concat(),
    };

    let mut in_place_mod_mpt = FakeSnapshotMptDb::default();
    in_place_mod_mpt.reset(/* in_place_mode = */ true);
    MptMerger::new(None, &mut in_place_mod_mpt)
        .merge(&mpt_kv_iter)
        .unwrap();

    let mpt_kv_iter = DumpedMptKvIterator {
        kv: [
            keys_unchanged
                .iter()
                .map(|k| (Vec::<u8>::from(&k[..]), Box::<[u8]>::from(&k[..])))
                .collect::<Vec<_>>(),
            keys_new
                .iter()
                .map(|k| (Vec::<u8>::from(&k[..]), Box::<[u8]>::from(&k[..])))
                .collect::<Vec<_>>(),
            keys_overwritten
                .iter()
                .map(|k| (Vec::<u8>::from(&k[..]), Box::<[u8]>::from(&k[..])))
                .collect::<Vec<_>>(),
        ]
        .concat(),
    };
    let mut new_snapshot_mpt = FakeSnapshotMptDb::default();
    new_snapshot_mpt.reset(/* in_place_mode = */ true);
    let supposed_merkle_root = MptMerger::new(None, &mut new_snapshot_mpt)
        .merge(&mpt_kv_iter)
        .unwrap();
    let new_snapshot_db = Arc::new(Mutex::new(FakeSnapshotDb::new(
        &mpt_kv_iter.kv,
        &new_snapshot_mpt,
    )));
    verify_snapshot_db(&new_snapshot_db);

    let delta_mpt_iter = DumpedMptKvIterator {
        kv: [
            keys_delete
                .iter()
                .map(|k| (Vec::<u8>::from(&k[..]), Box::<[u8]>::default()))
                .collect::<Vec<_>>(),
            keys_new
                .iter()
                .map(|k| (Vec::<u8>::from(&k[..]), Box::<[u8]>::from(&k[..])))
                .collect::<Vec<_>>(),
            keys_overwritten
                .iter()
                .map(|k| (Vec::<u8>::from(&k[..]), Box::<[u8]>::from(&k[..])))
                .collect::<Vec<_>>(),
        ]
        .concat(),
    };
    let mut save_as_mode_mpt = FakeSnapshotMptDb::default();
    let new_merkle_root =
        MptMerger::new(Some(&mut in_place_mod_mpt), &mut save_as_mode_mpt)
            .merge(&delta_mpt_iter)
            .unwrap();
    // Verify the mpt node structure.
    // FIXME: uncomment when the known bug is fixed.
    /*
    let save_as_mode_snapshot_db = Arc::new(Mutex::new(FakeSnapshotDb::new(
        &mpt_kv_iter.kv,
        &save_as_mode_mpt,
    )));
    verify_snapshot_db(&save_as_mode_snapshot_db);
    */
    assert_eq!(new_merkle_root, supposed_merkle_root);
    new_snapshot_mpt.assert_eq(&save_as_mode_mpt);

    in_place_mod_mpt.reset(/* in_place_mode */ true);
    let new_merkle_root = MptMerger::new(None, &mut in_place_mod_mpt)
        .merge(&delta_mpt_iter)
        .unwrap();
    let in_place_mode_snapshot_db = Arc::new(Mutex::new(FakeSnapshotDb::new(
        &mpt_kv_iter.kv,
        &in_place_mod_mpt,
    )));
    verify_snapshot_db(&in_place_mode_snapshot_db);
    assert_eq!(new_merkle_root, supposed_merkle_root);
    in_place_mod_mpt.assert_eq(&new_snapshot_mpt);
}

#[cfg(test)]
#[test]
fn test_two_way_merge() {
    let keys: Vec<Vec<u8>> = generate_keys(TEST_NUMBER_OF_KEYS);
    let set_size = TEST_NUMBER_OF_KEYS / 10;
    let (keys_unchanged, keys_overwritten, keys_delete, keys_new) = (
        &keys[0..set_size],
        &keys[set_size..set_size * 2],
        &keys[set_size * 2..set_size * 3],
        &keys[set_size * 3..set_size * 4],
    );

    let mpt_kv_iter = DumpedMptKvIterator {
        kv: [
            keys_unchanged
                .iter()
                .map(|k| (Vec::<u8>::from(&k[..]), Box::<[u8]>::from(&k[..])))
                .collect::<Vec<_>>(),
            keys_delete
                .iter()
                .map(|k| (Vec::<u8>::from(&k[..]), Box::<[u8]>::from(&k[..])))
                .collect::<Vec<_>>(),
            keys_overwritten
                .iter()
                .map(|k| (Vec::<u8>::from(&k[..]), Box::<[u8]>::from(&k[0..2])))
                .collect::<Vec<_>>(),
        ]
        .concat(),
    };

    let mut in_place_mod_mpt = FakeSnapshotMptDb::default();
    in_place_mod_mpt.reset(/* in_place_mode = */ true);
    MptMerger::new(None, &mut in_place_mod_mpt)
        .merge(&mpt_kv_iter)
        .unwrap();

    // One way merge.
    let delta_mpt_iter = DumpedMptKvIterator {
        kv: [
            keys_delete
                .iter()
                .map(|k| (Vec::<u8>::from(&k[..]), Box::<[u8]>::default()))
                .collect::<Vec<_>>(),
            keys_new
                .iter()
                .map(|k| (Vec::<u8>::from(&k[..]), Box::<[u8]>::from(&k[..])))
                .collect::<Vec<_>>(),
            keys_overwritten
                .iter()
                .map(|k| (Vec::<u8>::from(&k[..]), Box::<[u8]>::from(&k[..])))
                .collect::<Vec<_>>(),
        ]
        .concat(),
    };
    let mut save_as_mode_mpt = FakeSnapshotMptDb::default();
    let supposed_merkle_root =
        MptMerger::new(Some(&mut in_place_mod_mpt), &mut save_as_mode_mpt)
            .merge(&delta_mpt_iter)
            .unwrap();

    // Two way merge.
    in_place_mod_mpt.reset(/* in_place_mode */ true);
    let mut keys_deletion = Vec::from(keys_delete);
    keys_deletion.sort();
    let deletion = keys_deletion
        .iter()
        .map(|k| Ok((Vec::<u8>::from(&k[..]), ())))
        .collect::<Vec<_>>();
    let mut keys_insertion = [keys_new, keys_overwritten].concat();
    keys_insertion.sort();
    let insertion = keys_insertion
        .iter()
        .map(|k| Ok((Vec::<u8>::from(&k[..]), Box::<[u8]>::from(&k[..]))))
        .collect::<Vec<_>>();

    let new_merkle_root = MptMerger::new(None, &mut in_place_mod_mpt)
        .merge_insertion_deletion_separated(
            fallible_iterator::convert(deletion.into_iter()),
            fallible_iterator::convert(insertion.into_iter()),
            false,
        )
        .unwrap();

    // Merge result should be the same.
    assert_eq!(new_merkle_root, supposed_merkle_root);
    in_place_mod_mpt.assert_eq(&save_as_mode_mpt);
}

#[allow(unused)]
fn test_delta_subtree_size() {
    // FIXME: complete this test.
    unimplemented!()
}

use crate::{
    impls::{
        errors::*,
        merkle_patricia_trie::{CompressedPathRaw, CompressedPathTrait},
        storage_db::snapshot_mpt::{
            mpt_node_path_from_db_key, mpt_node_path_to_db_key,
        },
    },
    storage_db::{
        snapshot_mpt::CHECK_LOADED_SNAPSHOT_MPT_NODE, SnapshotMptIteraterTrait,
        SnapshotMptNode, SnapshotMptTraitRead, SnapshotMptTraitReadAndIterate,
        SnapshotMptTraitRw,
    },
};
use fallible_iterator::FallibleIterator;
use primitives::MerkleHash;
use std::{
    collections::{btree_map, BTreeMap, HashSet},
    ops::Bound::Excluded,
};

#[cfg(test)]
use crate::{
    impls::merkle_patricia_trie::{MptMerger, TrieNodeTrait},
    impls::storage_db::snapshot_mpt::tests::verify_snapshot_db,
    state_manager::StateManagerTrait,
    tests::{
        generate_keys, get_rng_for_test, new_state_manager_for_unit_test,
        snapshot::verifier::FakeSnapshotDb, DumpedMptKvIterator,
        TEST_NUMBER_OF_KEYS,
    },
    StateIndex, StorageStateTraitExt,
};
#[cfg(test)]
use parking_lot::Mutex;
#[cfg(test)]
use primitives::{EpochId, StorageKey, MERKLE_NULL_NODE};
#[cfg(test)]
use rand::Rng;
#[cfg(test)]
use std::sync::{atomic::Ordering, Arc};
