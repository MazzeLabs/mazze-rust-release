// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::{
    impls::{
        delta_mpt::DeltaMptIterator,
        errors::*,
        merkle_patricia_trie::{MptKeyValue, MptMerger},
        storage_db::snapshot_mpt::{
            SnapshotMpt, SnapshotMptIterableDb, SnapshotMptLoadNode,
        },
    },
    storage_db::{
        AlreadyOpenSnapshots, KeyValueDbIterableTrait, KeyValueDbTraitOwnedRead,
        KeyValueDbTraitRead, KeyValueDbTraitSingleWriter, KeyValueDbTypes,
        OpenSnapshotMptTrait, SnapshotDbTrait, SnapshotDbWriteableTrait,
        SnapshotMptDbTrait, SnapshotMptDbValue, SnapshotMptTraitReadAndIterate,
        SnapshotMptTraitRw,
    },
    utils::{
        tuple::ElementSatisfy,
        wrap::{Wrap, WrappedLifetimeFamily, WrappedTrait},
    },
    KVInserter,
};
use db::KeyValueStore;
use fallible_iterator::FallibleIterator;
use kvdb::DBTransaction;
use parking_lot::{Mutex, RwLock};
use primitives::{MerkleHash, StorageKeyWithSpace};
use std::{
    collections::HashMap,
    env,
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
};
use tokio::sync::Semaphore;

const PREFIX_KV: u8 = b'k';
const PREFIX_DELTA_SET: u8 = b's';
const PREFIX_DELTA_DEL: u8 = b'd';
const PREFIX_MPT: u8 = b'm';

lazy_static! {
    static ref OPEN_PARITYDB_CACHE: RwLock<
        HashMap<PathBuf, Weak<dyn KeyValueStore>>,
    > = RwLock::new(HashMap::new());
}

#[derive(Clone)]
pub struct PrefixedKvdbParitydb<ValueType> {
    kvdb: Arc<dyn KeyValueStore>,
    col: u32,
    prefix: u8,
    prefix_bytes: [u8; 1],
    transaction: Arc<Mutex<Option<DBTransaction>>>,
    _marker: PhantomData<ValueType>,
}

impl<ValueType> PrefixedKvdbParitydb<ValueType> {
    fn new(
        kvdb: Arc<dyn KeyValueStore>,
        col: u32,
        prefix: u8,
        transaction: Arc<Mutex<Option<DBTransaction>>>,
    ) -> Self {
        Self {
            kvdb,
            col,
            prefix,
            prefix_bytes: [prefix],
            transaction,
            _marker: Default::default(),
        }
    }

    fn prefixed_key(&self, key: &[u8]) -> Vec<u8> {
        let mut prefixed = Vec::with_capacity(1 + key.len());
        prefixed.push(self.prefix);
        prefixed.extend_from_slice(key);
        prefixed
    }
}

impl KeyValueDbTypes for PrefixedKvdbParitydb<Box<[u8]>> {
    type ValueType = Box<[u8]>;
}

impl KeyValueDbTraitRead for PrefixedKvdbParitydb<Box<[u8]>> {
    fn get(&self, key: &[u8]) -> Result<Option<Self::ValueType>> {
        let prefixed_key = self.prefixed_key(key);
        Ok(self
            .kvdb
            .get(self.col, &prefixed_key)?
            .map(|v| v.into_boxed_slice()))
    }
}

impl KeyValueDbTraitOwnedRead for PrefixedKvdbParitydb<Box<[u8]>> {
    fn get_mut(&mut self, key: &[u8]) -> Result<Option<Self::ValueType>> {
        self.get(key)
    }
}

impl KeyValueDbTraitSingleWriter for PrefixedKvdbParitydb<Box<[u8]>> {
    fn delete(&mut self, key: &[u8]) -> Result<Option<Option<Self::ValueType>>> {
        let prefixed_key = self.prefixed_key(key);
        let mut tx_guard = self.transaction.lock();
        if let Some(tx) = tx_guard.as_mut() {
            tx.delete(self.col, &prefixed_key);
        } else {
            let mut tx = self.kvdb.transaction();
            tx.delete(self.col, &prefixed_key);
            self.kvdb.write(tx)?;
        }
        Ok(None)
    }

    fn put(
        &mut self, key: &[u8], value: &<Self::ValueType as crate::storage_db::DbValueType>::Type,
    ) -> Result<Option<Option<Self::ValueType>>> {
        let prefixed_key = self.prefixed_key(key);
        let mut tx_guard = self.transaction.lock();
        if let Some(tx) = tx_guard.as_mut() {
            tx.put(self.col, &prefixed_key, value);
        } else {
            let mut tx = self.kvdb.transaction();
            tx.put(self.col, &prefixed_key, value);
            self.kvdb.write(tx)?;
        }
        Ok(None)
    }
}

impl KeyValueDbTypes for PrefixedKvdbParitydb<()> {
    type ValueType = ();
}

impl KeyValueDbTraitRead for PrefixedKvdbParitydb<()> {
    fn get(&self, key: &[u8]) -> Result<Option<Self::ValueType>> {
        let prefixed_key = self.prefixed_key(key);
        Ok(self.kvdb.get(self.col, &prefixed_key)?.map(|_| ()))
    }
}

impl KeyValueDbTraitOwnedRead for PrefixedKvdbParitydb<()> {
    fn get_mut(&mut self, key: &[u8]) -> Result<Option<Self::ValueType>> {
        self.get(key)
    }
}

impl KeyValueDbTraitSingleWriter for PrefixedKvdbParitydb<()> {
    fn delete(&mut self, key: &[u8]) -> Result<Option<Option<Self::ValueType>>> {
        let prefixed_key = self.prefixed_key(key);
        let mut tx_guard = self.transaction.lock();
        if let Some(tx) = tx_guard.as_mut() {
            tx.delete(self.col, &prefixed_key);
        } else {
            let mut tx = self.kvdb.transaction();
            tx.delete(self.col, &prefixed_key);
            self.kvdb.write(tx)?;
        }
        Ok(None)
    }

    fn put(
        &mut self, key: &[u8], _value: &<Self::ValueType as crate::storage_db::DbValueType>::Type,
    ) -> Result<Option<Option<Self::ValueType>>> {
        let prefixed_key = self.prefixed_key(key);
        let mut tx_guard = self.transaction.lock();
        if let Some(tx) = tx_guard.as_mut() {
            tx.put(self.col, &prefixed_key, &[]);
        } else {
            let mut tx = self.kvdb.transaction();
            tx.put(self.col, &prefixed_key, &[]);
            self.kvdb.write(tx)?;
        }
        Ok(None)
    }
}

pub struct KvdbParitydbIteratorTag;

pub struct ParitydbRangeIter<'a, ValueType> {
    iter: Box<dyn Iterator<Item = (Box<[u8]>, Box<[u8]>)> + 'a>,
    prefix: u8,
    lower_bound: Option<Vec<u8>>,
    lower_exclusive: bool,
    upper_bound: Option<Vec<u8>>,
    _marker: PhantomData<ValueType>,
}

impl<'a, ValueType> ParitydbRangeIter<'a, ValueType> {
    fn new(
        iter: Box<dyn Iterator<Item = (Box<[u8]>, Box<[u8]>)> + 'a>,
        prefix: u8,
        lower_bound: Option<Vec<u8>>,
        lower_exclusive: bool,
        upper_bound: Option<Vec<u8>>,
    ) -> Self {
        Self {
            iter,
            prefix,
            lower_bound,
            lower_exclusive,
            upper_bound,
            _marker: Default::default(),
        }
    }

    fn strip_prefix<'k>(&self, key: &'k [u8]) -> Option<&'k [u8]> {
        if key.first().copied() == Some(self.prefix) {
            Some(&key[1..])
        } else {
            None
        }
    }
}

impl<'a> FallibleIterator for ParitydbRangeIter<'a, Box<[u8]>> {
    type Item = (Vec<u8>, Box<[u8]>);
    type Error = Error;

    fn next(&mut self) -> Result<Option<Self::Item>> {
        loop {
            let (key, value) = match self.iter.next() {
                None => return Ok(None),
                Some(kv) => kv,
            };
            let stripped = match self.strip_prefix(&key) {
                None => return Ok(None),
                Some(k) => k,
            };
            if let Some(lower) = &self.lower_bound {
                if self.lower_exclusive {
                    if stripped <= lower.as_slice() {
                        continue;
                    }
                } else if stripped < lower.as_slice() {
                    continue;
                }
            }
            if let Some(upper) = &self.upper_bound {
                if stripped >= upper.as_slice() {
                    return Ok(None);
                }
            }
            return Ok(Some((stripped.to_vec(), value)));
        }
    }
}

impl<'a> FallibleIterator for ParitydbRangeIter<'a, ()> {
    type Item = (Vec<u8>, ());
    type Error = Error;

    fn next(&mut self) -> Result<Option<Self::Item>> {
        loop {
            let (key, _value) = match self.iter.next() {
                None => return Ok(None),
                Some(kv) => kv,
            };
            let stripped = match self.strip_prefix(&key) {
                None => return Ok(None),
                Some(k) => k,
            };
            if let Some(lower) = &self.lower_bound {
                if self.lower_exclusive {
                    if stripped <= lower.as_slice() {
                        continue;
                    }
                } else if stripped < lower.as_slice() {
                    continue;
                }
            }
            if let Some(upper) = &self.upper_bound {
                if stripped >= upper.as_slice() {
                    return Ok(None);
                }
            }
            return Ok(Some((stripped.to_vec(), ())));
        }
    }
}

impl<'a>
    WrappedLifetimeFamily<
        'a,
        dyn FallibleIterator<Item = (Vec<u8>, Box<[u8]>), Error = Error>,
    > for KvdbIterIterator<(Vec<u8>, Box<[u8]>), [u8], KvdbParitydbIteratorTag>
{
    type Out = ParitydbRangeIter<'a, Box<[u8]>>;
}

impl WrappedTrait<
        dyn FallibleIterator<Item = (Vec<u8>, Box<[u8]>), Error = Error>,
    > for KvdbIterIterator<(Vec<u8>, Box<[u8]>), [u8], KvdbParitydbIteratorTag>
{
}

impl<'a>
    WrappedLifetimeFamily<
        'a,
        dyn FallibleIterator<Item = (Vec<u8>, ()), Error = Error>,
    > for KvdbIterIterator<(Vec<u8>, ()), [u8], KvdbParitydbIteratorTag>
{
    type Out = ParitydbRangeIter<'a, ()>;
}

impl WrappedTrait<dyn FallibleIterator<Item = (Vec<u8>, ()), Error = Error>>
    for KvdbIterIterator<(Vec<u8>, ()), [u8], KvdbParitydbIteratorTag>
{
}

impl KeyValueDbIterableTrait<MptKeyValue, [u8], KvdbParitydbIteratorTag>
    for PrefixedKvdbParitydb<Box<[u8]>>
where
    KvdbIterIterator<MptKeyValue, [u8], KvdbParitydbIteratorTag>:
        WrappedTrait<dyn FallibleIterator<Item = MptKeyValue, Error = Error>>
            + for<'a> WrappedLifetimeFamily<
                'a,
                dyn FallibleIterator<Item = MptKeyValue, Error = Error>,
                Out = ParitydbRangeIter<'a, Box<[u8]>>,
            >,
{
    fn iter_range(
        &mut self, lower_bound_incl: &[u8], upper_bound_excl: Option<&[u8]>,
    ) -> Result<
        Wrap<
            KvdbIterIterator<MptKeyValue, [u8], KvdbParitydbIteratorTag>,
            dyn FallibleIterator<Item = MptKeyValue, Error = Error>,
        >,
    > {
        let iter = self.kvdb.iter_from_prefix(self.col, &self.prefix_bytes);
        let lower = if lower_bound_incl.is_empty() {
            None
        } else {
            Some(lower_bound_incl.to_vec())
        };
        let upper = upper_bound_excl.map(|b| b.to_vec());
        Ok(Wrap(ParitydbRangeIter::new(
            iter,
            self.prefix,
            lower,
            false,
            upper,
        )))
    }

    fn iter_range_excl(
        &mut self, lower_bound_excl: &[u8], upper_bound_excl: &[u8],
    ) -> Result<
        Wrap<
            KvdbIterIterator<MptKeyValue, [u8], KvdbParitydbIteratorTag>,
            dyn FallibleIterator<Item = MptKeyValue, Error = Error>,
        >,
    > {
        let iter = self.kvdb.iter_from_prefix(self.col, &self.prefix_bytes);
        let lower = if lower_bound_excl.is_empty() {
            None
        } else {
            Some(lower_bound_excl.to_vec())
        };
        Ok(Wrap(ParitydbRangeIter::new(
            iter,
            self.prefix,
            lower,
            true,
            Some(upper_bound_excl.to_vec()),
        )))
    }
}

impl KeyValueDbIterableTrait<(Vec<u8>, ()), [u8], KvdbParitydbIteratorTag>
    for PrefixedKvdbParitydb<()>
where
    KvdbIterIterator<(Vec<u8>, ()), [u8], KvdbParitydbIteratorTag>:
        WrappedTrait<
                dyn FallibleIterator<Item = (Vec<u8>, ()), Error = Error>,
            > + for<'a> WrappedLifetimeFamily<
                'a,
                dyn FallibleIterator<Item = (Vec<u8>, ()), Error = Error>,
                Out = ParitydbRangeIter<'a, ()>,
            >,
{
    fn iter_range(
        &mut self, lower_bound_incl: &[u8], upper_bound_excl: Option<&[u8]>,
    ) -> Result<
        Wrap<
            KvdbIterIterator<(Vec<u8>, ()), [u8], KvdbParitydbIteratorTag>,
            dyn FallibleIterator<Item = (Vec<u8>, ()), Error = Error>,
        >,
    > {
        let iter = self.kvdb.iter_from_prefix(self.col, &self.prefix_bytes);
        let lower = if lower_bound_incl.is_empty() {
            None
        } else {
            Some(lower_bound_incl.to_vec())
        };
        let upper = upper_bound_excl.map(|b| b.to_vec());
        Ok(Wrap(ParitydbRangeIter::new(
            iter,
            self.prefix,
            lower,
            false,
            upper,
        )))
    }

    fn iter_range_excl(
        &mut self, lower_bound_excl: &[u8], upper_bound_excl: &[u8],
    ) -> Result<
        Wrap<
            KvdbIterIterator<(Vec<u8>, ()), [u8], KvdbParitydbIteratorTag>,
            dyn FallibleIterator<Item = (Vec<u8>, ()), Error = Error>,
        >,
    > {
        let iter = self.kvdb.iter_from_prefix(self.col, &self.prefix_bytes);
        let lower = if lower_bound_excl.is_empty() {
            None
        } else {
            Some(lower_bound_excl.to_vec())
        };
        Ok(Wrap(ParitydbRangeIter::new(
            iter,
            self.prefix,
            lower,
            true,
            Some(upper_bound_excl.to_vec()),
        )))
    }
}

impl ElementSatisfy<
        dyn KeyValueDbIterableTrait<MptKeyValue, [u8], KvdbParitydbIteratorTag>,
    > for PrefixedKvdbParitydb<Box<[u8]>>
{
    fn to_constrain_object(
        &self,
    ) -> &(dyn KeyValueDbIterableTrait<MptKeyValue, [u8], KvdbParitydbIteratorTag>
          + 'static) {
        self
    }

    fn to_constrain_object_mut(
        &mut self,
    ) -> &mut (dyn KeyValueDbIterableTrait<
        MptKeyValue,
        [u8],
        KvdbParitydbIteratorTag,
    > + 'static) {
        self
    }
}

impl
    WrappedLifetimeFamily<
        '_,
        dyn KeyValueDbIterableTrait<MptKeyValue, [u8], KvdbParitydbIteratorTag>,
    > for PrefixedKvdbParitydb<Box<[u8]>>
{
    type Out = Self;
}

impl WrappedTrait<
        dyn KeyValueDbIterableTrait<MptKeyValue, [u8], KvdbParitydbIteratorTag>,
    > for PrefixedKvdbParitydb<Box<[u8]>>
{
}

impl SnapshotMptLoadNode for PrefixedKvdbParitydb<SnapshotMptDbValue> {
    fn load_node_rlp(
        &mut self, key: &[u8],
    ) -> Result<Option<SnapshotMptDbValue>> {
        self.get_mut(key)
    }
}

impl SnapshotMptIterableDb for PrefixedKvdbParitydb<SnapshotMptDbValue> {
    type IterTag = KvdbParitydbIteratorTag;
}

pub struct SnapshotKvDbParitydb {
    kvdb: Arc<dyn KeyValueStore>,
    col: u32,
    path: PathBuf,
    open_semaphore: Arc<Semaphore>,
    release_semaphore_on_drop: bool,
    remove_on_close: AtomicBool,
    mpt_table_in_current_db: bool,
    transaction: Arc<Mutex<Option<DBTransaction>>>,
}

impl SnapshotKvDbParitydb {
    pub const SNAPSHOT_DB_PARITYDB_DIR_PREFIX: &'static str = "paritydb_";
    const DB_COLUMNS: u32 = 1;

    fn cached_kvdb(snapshot_path: &Path) -> Option<Arc<dyn KeyValueStore>> {
        let mut cache = OPEN_PARITYDB_CACHE.write();
        if let Some(existing) = cache.get(snapshot_path) {
            if let Some(kvdb) = existing.upgrade() {
                return Some(kvdb);
            }
            cache.remove(snapshot_path);
        }
        None
    }

    fn cache_kvdb(snapshot_path: &Path, kvdb: &Arc<dyn KeyValueStore>) {
        OPEN_PARITYDB_CACHE
            .write()
            .insert(snapshot_path.to_path_buf(), Arc::downgrade(kvdb));
    }

    fn maybe_drop_cached_kvdb(&self) {
        if Arc::strong_count(&self.kvdb) == 1 {
            OPEN_PARITYDB_CACHE.write().remove(&self.path);
        }
    }

    fn from_kvdb(
        kvdb: Arc<dyn KeyValueStore>, path: &Path,
        open_semaphore: &Arc<Semaphore>,
    ) -> SnapshotKvDbParitydb {
        SnapshotKvDbParitydb {
            kvdb,
            col: 0,
            path: path.to_path_buf(),
            open_semaphore: Arc::clone(open_semaphore),
            release_semaphore_on_drop: true,
            remove_on_close: AtomicBool::new(false),
            mpt_table_in_current_db: true,
            transaction: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_remove_on_last_close(&self) {
        self.remove_on_close.store(true, Ordering::Relaxed);
    }

    fn prefixed_kv_db(&self, prefix: u8) -> PrefixedKvdbParitydb<Box<[u8]>> {
        PrefixedKvdbParitydb::new(
            Arc::clone(&self.kvdb),
            self.col,
            prefix,
            Arc::clone(&self.transaction),
        )
    }

    fn prefixed_unit_db(&self, prefix: u8) -> PrefixedKvdbParitydb<()> {
        PrefixedKvdbParitydb::new(
            Arc::clone(&self.kvdb),
            self.col,
            prefix,
            Arc::clone(&self.transaction),
        )
    }

    fn apply_update_to_kvdb(&mut self) -> Result<()> {
        let mut delete_iter = self.dumped_delta_kv_delete_keys_iterator()?;
        let mut set_iter = self.dumped_delta_kv_set_keys_iterator()?;

        let mut deletes = Vec::new();
        let mut set_entries = Vec::new();

        let mut del_it = delete_iter.iter_range(&[], None)?.take();
        while let Some((key, _)) = del_it.next()? {
            deletes.push(key);
        }

        let mut set_it = set_iter.iter_range(&[], None)?.take();
        while let Some((key, value)) = set_it.next()? {
            set_entries.push((key, value));
        }

        SnapshotDbWriteableTrait::start_transaction(self)?;
        for key in deletes {
            self.delete(&key)?;
        }
        for (key, value) in set_entries {
            self.put(&key, &value)?;
        }
        SnapshotDbWriteableTrait::commit_transaction(self)?;
        Ok(())
    }

    pub fn dumped_delta_kv_set_keys_iterator(
        &self,
    ) -> Result<PrefixedKvdbParitydb<Box<[u8]>>> {
        Ok(self.prefixed_kv_db(PREFIX_DELTA_SET))
    }

    pub fn dumped_delta_kv_delete_keys_iterator(
        &self,
    ) -> Result<PrefixedKvdbParitydb<()>> {
        Ok(self.prefixed_unit_db(PREFIX_DELTA_DEL))
    }

    pub fn dump_delta_mpt(&mut self, delta_mpt: &DeltaMptIterator) -> Result<()> {
        SnapshotDbWriteableTrait::start_transaction(self)?;
        delta_mpt.iterate(&mut DeltaMptMergeDumperParitydb {
            set_db: self.prefixed_kv_db(PREFIX_DELTA_SET),
            delete_db: self.prefixed_unit_db(PREFIX_DELTA_DEL),
        })?;
        SnapshotDbWriteableTrait::commit_transaction(self)?;
        Ok(())
    }

    pub fn drop_delta_mpt_dump(&mut self) -> Result<()> {
        let mut set_iter = self.dumped_delta_kv_set_keys_iterator()?;
        let mut del_iter = self.dumped_delta_kv_delete_keys_iterator()?;

        let mut set_keys = Vec::new();
        let mut del_keys = Vec::new();

        let mut it = set_iter.iter_range(&[], None)?.take();
        while let Some((key, _)) = it.next()? {
            set_keys.push(key);
        }

        let mut it = del_iter.iter_range(&[], None)?.take();
        while let Some((key, _)) = it.next()? {
            del_keys.push(key);
        }

        SnapshotDbWriteableTrait::start_transaction(self)?;
        for key in set_keys {
            self.prefixed_kv_db(PREFIX_DELTA_SET).delete(&key)?;
        }
        for key in del_keys {
            self.prefixed_unit_db(PREFIX_DELTA_DEL).delete(&key)?;
        }
        SnapshotDbWriteableTrait::commit_transaction(self)?;
        Ok(())
    }

    pub fn drop_mpt_table(&mut self) -> Result<()> {
        // MPT is stored in the same database with prefix, drop is not supported.
        Ok(())
    }

    fn snapshot_mpt_iterator(
        &self,
    ) -> Result<
        Wrap<
            PrefixedKvdbParitydb<Box<[u8]>>,
            dyn KeyValueDbIterableTrait<MptKeyValue, [u8], KvdbParitydbIteratorTag>,
        >,
    > {
        Ok(Wrap(self.prefixed_kv_db(PREFIX_MPT)))
    }
}

impl Drop for SnapshotKvDbParitydb {
    fn drop(&mut self) {
        if self.remove_on_close.load(Ordering::Relaxed) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
        self.maybe_drop_cached_kvdb();
        if self.release_semaphore_on_drop {
            self.open_semaphore.add_permits(1);
        }
    }
}

impl KeyValueDbTypes for SnapshotKvDbParitydb {
    type ValueType = Box<[u8]>;
}

impl KeyValueDbTraitRead for SnapshotKvDbParitydb {
    fn get(&self, key: &[u8]) -> Result<Option<Self::ValueType>> {
        self.prefixed_kv_db(PREFIX_KV).get(key)
    }
}

impl KeyValueDbTraitOwnedRead for SnapshotKvDbParitydb {
    fn get_mut(&mut self, key: &[u8]) -> Result<Option<Self::ValueType>> {
        self.get(key)
    }
}

impl KeyValueDbTraitSingleWriter for SnapshotKvDbParitydb {
    fn delete(&mut self, key: &[u8]) -> Result<Option<Option<Self::ValueType>>> {
        self.prefixed_kv_db(PREFIX_KV).delete(key)
    }

    fn put(
        &mut self, key: &[u8], value: &<Self::ValueType as crate::storage_db::DbValueType>::Type,
    ) -> Result<Option<Option<Self::ValueType>>> {
        self.prefixed_kv_db(PREFIX_KV).put(key, value)
    }
}

impl SnapshotDbWriteableTrait for SnapshotKvDbParitydb {
    type SnapshotDbBorrowMutType = SnapshotMpt<
        PrefixedKvdbParitydb<SnapshotMptDbValue>,
        PrefixedKvdbParitydb<SnapshotMptDbValue>,
    >;

    fn start_transaction(&mut self) -> Result<()> {
        let mut guard = self.transaction.lock();
        if guard.is_some() {
            return Ok(());
        }
        *guard = Some(self.kvdb.transaction());
        Ok(())
    }

    fn commit_transaction(&mut self) -> Result<()> {
        let mut guard = self.transaction.lock();
        if let Some(tx) = guard.take() {
            self.kvdb.write(tx)?;
        }
        Ok(())
    }

    fn put_kv(
        &mut self, key: &[u8], value: &<Self::ValueType as crate::storage_db::DbValueType>::Type,
    ) -> Result<Option<Option<Self::ValueType>>> {
        self.put(key, value)
    }

    fn open_snapshot_mpt_owned(
        &mut self,
    ) -> Result<Self::SnapshotDbBorrowMutType> {
        SnapshotMpt::new(self.prefixed_kv_db(PREFIX_MPT))
    }
}

impl SnapshotMptDbTrait for SnapshotKvDbParitydb {
    fn start_transaction(&mut self) -> Result<()> {
        <SnapshotKvDbParitydb as SnapshotDbTrait>::start_transaction(self)
    }

    fn commit_transaction(&mut self) -> Result<()> {
        <SnapshotKvDbParitydb as SnapshotDbTrait>::commit_transaction(self)
    }
}

impl<'db> OpenSnapshotMptTrait<'db> for SnapshotKvDbParitydb {
    type SnapshotDbAsOwnedType = SnapshotMpt<
        PrefixedKvdbParitydb<SnapshotMptDbValue>,
        PrefixedKvdbParitydb<SnapshotMptDbValue>,
    >;
    type SnapshotDbBorrowMutType = SnapshotMpt<
        PrefixedKvdbParitydb<SnapshotMptDbValue>,
        PrefixedKvdbParitydb<SnapshotMptDbValue>,
    >;
    type SnapshotDbBorrowSharedType = SnapshotMpt<
        PrefixedKvdbParitydb<SnapshotMptDbValue>,
        PrefixedKvdbParitydb<SnapshotMptDbValue>,
    >;

    fn open_snapshot_mpt_owned(
        &'db mut self,
    ) -> Result<Self::SnapshotDbBorrowMutType> {
        SnapshotMpt::new(self.prefixed_kv_db(PREFIX_MPT))
    }

    fn open_snapshot_mpt_as_owned(
        &'db self,
    ) -> Result<Self::SnapshotDbAsOwnedType> {
        SnapshotMpt::new(self.prefixed_kv_db(PREFIX_MPT))
    }

    fn open_snapshot_mpt_shared(
        &'db self,
    ) -> Result<Self::SnapshotDbBorrowSharedType> {
        SnapshotMpt::new(self.prefixed_kv_db(PREFIX_MPT))
    }
}

impl SnapshotDbTrait for SnapshotKvDbParitydb {
    type SnapshotKvdbIterTraitTag = KvdbParitydbIteratorTag;
    type SnapshotKvdbIterType = PrefixedKvdbParitydb<Box<[u8]>>;
    type SnapshotMptDb = SnapshotKvDbParitydb;

    fn get_null_snapshot() -> Self {
        lazy_static! {
            static ref NULL_SNAPSHOT_STORE: (Arc<dyn KeyValueStore>, PathBuf) =
                {
                    let null_path = env::temp_dir().join(format!(
                        "mazze_null_snapshot_{}",
                        std::process::id()
                    ));
                    let parity_config = db::ParityDbOpenConfig {
                        columns: SnapshotKvDbParitydb::DB_COLUMNS,
                        compression: None,
                        disable_wal: false,
                        stats: false,
                    };
                    let settings = db::paritydb_settings(
                        null_path.clone(),
                        &parity_config,
                    )
                    .expect("paritydb settings");
                    let db =
                        db::open_database(&settings).expect("open paritydb");
                    (db.key_value(), null_path)
                };
        }
        let (kvdb, null_path) = &*NULL_SNAPSHOT_STORE;
        SnapshotKvDbParitydb {
            kvdb: Arc::clone(kvdb),
            col: 0,
            path: null_path.clone(),
            open_semaphore: Arc::new(Semaphore::new(0)),
            release_semaphore_on_drop: false,
            remove_on_close: AtomicBool::new(false),
            mpt_table_in_current_db: true,
            transaction: Arc::new(Mutex::new(None)),
        }
    }

    fn open(
        snapshot_path: &Path, readonly: bool,
        _already_open_snapshots: &AlreadyOpenSnapshots<Self>,
        open_semaphore: &Arc<Semaphore>,
    ) -> Result<SnapshotKvDbParitydb> {
        if readonly && !snapshot_path.exists() {
            bail!(ErrorKind::SnapshotNotFound);
        }
        if let Some(kvdb) = Self::cached_kvdb(snapshot_path) {
            return Ok(Self::from_kvdb(kvdb, snapshot_path, open_semaphore));
        }
        let parity_config = db::ParityDbOpenConfig {
            columns: Self::DB_COLUMNS,
            compression: None,
            disable_wal: false,
            stats: false,
        };
        let settings =
            db::paritydb_settings(snapshot_path.to_path_buf(), &parity_config)?;
        match db::open_database(&settings) {
            Ok(db) => {
                let kvdb = db.key_value();
                Self::cache_kvdb(snapshot_path, &kvdb);
                Ok(Self::from_kvdb(kvdb, snapshot_path, open_semaphore))
            }
            Err(err) => {
                if let Some(kvdb) = Self::cached_kvdb(snapshot_path) {
                    return Ok(Self::from_kvdb(
                        kvdb,
                        snapshot_path,
                        open_semaphore,
                    ));
                }
                Err(err.into())
            }
        }
    }

    fn create(
        snapshot_path: &Path,
        _already_open_snapshots: &AlreadyOpenSnapshots<Self>,
        open_semaphore: &Arc<Semaphore>,
        _mpt_table_in_current_db: bool,
    ) -> Result<SnapshotKvDbParitydb> {
        if snapshot_path.exists() {
            bail!(ErrorKind::SnapshotAlreadyExists);
        }
        std::fs::create_dir_all(snapshot_path)?;
        let parity_config = db::ParityDbOpenConfig {
            columns: Self::DB_COLUMNS,
            compression: None,
            disable_wal: false,
            stats: false,
        };
        let settings =
            db::paritydb_settings(snapshot_path.to_path_buf(), &parity_config)?;
        let db = db::open_database(&settings)?;
        let kvdb = db.key_value();
        Self::cache_kvdb(snapshot_path, &kvdb);
        Ok(Self::from_kvdb(kvdb, snapshot_path, open_semaphore))
    }

    fn direct_merge(
        &mut self, old_snapshot_db: Option<&Arc<SnapshotKvDbParitydb>>,
        _mpt_snapshot: &mut Option<SnapshotKvDbParitydb>,
        recover_mpt_with_kv_snapshot_exist: bool,
        in_reconstruct_snapshot_state: bool,
    ) -> Result<MerkleHash> {
        if !recover_mpt_with_kv_snapshot_exist {
            self.apply_update_to_kvdb()?;
        }

        if let Some(old_db) = old_snapshot_db {
            let mut key_value_iter = old_db.snapshot_mpt_iterator()?.take();
            let mut kv_iter = key_value_iter.iter_range(&[], None)?.take();
            let mut new_mpt = self.prefixed_kv_db(PREFIX_MPT);
            SnapshotDbWriteableTrait::start_transaction(self)?;
            while let Some((access_key, expected_value)) = kv_iter.next()? {
                new_mpt.put(&access_key, &expected_value)?;
            }
            SnapshotDbWriteableTrait::commit_transaction(self)?;
        }

        let mut set_keys_iter = self.dumped_delta_kv_set_keys_iterator()?;
        let mut delete_keys_iter =
            self.dumped_delta_kv_delete_keys_iterator()?;

        SnapshotDbWriteableTrait::start_transaction(self)?;
        let mut mpt_to_modify =
            SnapshotDbWriteableTrait::open_snapshot_mpt_owned(self)?;
        let mut mpt_merger = MptMerger::new(
            None,
            &mut mpt_to_modify as &mut dyn SnapshotMptTraitRw,
        );

        let snapshot_root = mpt_merger.merge_insertion_deletion_separated(
            delete_keys_iter.iter_range(&[], None)?.take(),
            set_keys_iter.iter_range(&[], None)?.take(),
            in_reconstruct_snapshot_state,
        )?;
        SnapshotDbWriteableTrait::commit_transaction(self)?;

        Ok(snapshot_root)
    }

    fn copy_and_merge(
        &mut self, old_snapshot_db: &Arc<SnapshotKvDbParitydb>,
        _mpt_snapshot_db: &mut Option<SnapshotKvDbParitydb>,
        in_reconstruct_snapshot_state: bool,
    ) -> Result<MerkleHash> {
        let mut kv_iter = old_snapshot_db.snapshot_kv_iterator()?.take();
        let mut iter = kv_iter.iter_range(&[], None)?.take();
        SnapshotDbWriteableTrait::start_transaction(self)?;
        while let Ok(kv_item) = iter.next() {
            match kv_item {
                Some((k, v)) => {
                    self.put(&k, &v)?;
                }
                None => break,
            }
        }
        SnapshotDbWriteableTrait::commit_transaction(self)?;
        self.apply_update_to_kvdb()?;

        let mut set_keys_iter = self.dumped_delta_kv_set_keys_iterator()?;
        let mut delete_keys_iter =
            self.dumped_delta_kv_delete_keys_iterator()?;

        let mut base_mpt = old_snapshot_db.open_snapshot_mpt_as_owned()?;
        let mut save_as_mpt =
            SnapshotDbWriteableTrait::open_snapshot_mpt_owned(self)?;

        SnapshotDbWriteableTrait::start_transaction(self)?;
        let mut mpt_merger = MptMerger::new(
            Some(&mut base_mpt as &mut dyn SnapshotMptTraitReadAndIterate),
            &mut save_as_mpt as &mut dyn SnapshotMptTraitRw,
        );
        let snapshot_root = mpt_merger.merge_insertion_deletion_separated(
            delete_keys_iter.iter_range(&[], None)?.take(),
            set_keys_iter.iter_range(&[], None)?.take(),
            in_reconstruct_snapshot_state,
        )?;
        SnapshotDbWriteableTrait::commit_transaction(self)?;

        Ok(snapshot_root)
    }

    fn start_transaction(&mut self) -> Result<()> {
        SnapshotDbWriteableTrait::start_transaction(self)
    }

    fn commit_transaction(&mut self) -> Result<()> {
        SnapshotDbWriteableTrait::commit_transaction(self)
    }

    fn is_mpt_table_in_current_db(&self) -> bool {
        self.mpt_table_in_current_db
    }

    fn snapshot_kv_iterator(
        &self,
    ) -> Result<
        Wrap<
            PrefixedKvdbParitydb<Box<[u8]>>,
            dyn KeyValueDbIterableTrait<MptKeyValue, [u8], KvdbParitydbIteratorTag>,
        >,
    > {
        Ok(Wrap(self.prefixed_kv_db(PREFIX_KV)))
    }
}

struct DeltaMptMergeDumperParitydb {
    set_db: PrefixedKvdbParitydb<Box<[u8]>>,
    delete_db: PrefixedKvdbParitydb<()>,
}

impl KVInserter<MptKeyValue> for DeltaMptMergeDumperParitydb {
    fn push(&mut self, x: MptKeyValue) -> Result<()> {
        let (mpt_key, value) = x;
        let snapshot_key =
            StorageKeyWithSpace::from_delta_mpt_key(&mpt_key).to_key_bytes();
        if value.len() > 0 {
            self.set_db.put(&snapshot_key, &value)?;
        } else {
            self.delete_db.put(&snapshot_key, &())?;
        }
        Ok(())
    }
}

use crate::storage_db::key_value_db::KvdbIterIterator;
