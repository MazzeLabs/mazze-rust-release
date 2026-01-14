// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

pub struct KvdbParitydb {
    pub kvdb: Arc<dyn KeyValueStore>,
    pub col: u32,
}

impl Clone for KvdbParitydb {
    fn clone(&self) -> Self {
        Self {
            kvdb: Arc::clone(&self.kvdb),
            col: self.col,
        }
    }
}

impl MallocSizeOf for KvdbParitydb {
    fn size_of(&self, _ops: &mut MallocSizeOfOps) -> usize {
        0
    }
}

pub struct KvdbParityDbTransaction {
    pending: DBTransaction,
    col: u32,
}

impl KeyValueDbTraitRead for KvdbParitydb {
    fn get(&self, key: &[u8]) -> Result<Option<Box<[u8]>>> {
        Ok(self
            .kvdb
            .get(self.col, key)?
            .map(|db_value| db_value.into_boxed_slice()))
    }
}

mark_kvdb_multi_reader!(KvdbParitydb);

impl KeyValueDbTypes for KvdbParitydb {
    type ValueType = Box<[u8]>;
}

impl KeyValueDbTrait for KvdbParitydb {
    fn delete(&self, key: &[u8]) -> Result<Option<Option<Box<[u8]>>>> {
        random_crash_if_enabled("paritydb delete");
        let mut transaction = self.kvdb.transaction();
        transaction.delete(self.col, key);
        self.kvdb.write(transaction)?;
        Ok(None)
    }

    fn put(
        &self, key: &[u8], value: &[u8],
    ) -> Result<Option<Option<Box<[u8]>>>> {
        random_crash_if_enabled("paritydb put");
        let mut transaction = self.kvdb.transaction();
        transaction.put(self.col, key, value);
        self.kvdb.write(transaction)?;
        Ok(None)
    }
}

impl KeyValueDbTypes for KvdbParityDbTransaction {
    type ValueType = Box<[u8]>;
}

impl KeyValueDbTraitSingleWriter for KvdbParityDbTransaction {
    fn delete(&mut self, key: &[u8]) -> Result<Option<Option<Box<[u8]>>>> {
        self.pending.delete(self.col, key);
        Ok(None)
    }

    fn put(
        &mut self, key: &[u8], value: &[u8],
    ) -> Result<Option<Option<Box<[u8]>>>> {
        self.pending.put(self.col, key, value);
        Ok(None)
    }
}

impl KeyValueDbTraitOwnedRead for KvdbParityDbTransaction {
    fn get_mut(&mut self, _key: &[u8]) -> Result<Option<Box<[u8]>>> {
        // DBTransaction doesn't implement get method, so the user shouldn't
        // rely on this method.
        unreachable!()
    }
}

impl KeyValueDbTransactionTrait for KvdbParityDbTransaction {
    fn commit(&mut self, db: &dyn Any) -> Result<()> {
        random_crash_if_enabled("paritydb commit");
        match db.downcast_ref::<KvdbParitydb>() {
            Some(as_kvdb_paritydb) => {
                let wrapped_ops = DBTransaction {
                    ops: self.pending.ops.clone(),
                };
                let result = as_kvdb_paritydb.kvdb.write(wrapped_ops);
                match result {
                    Ok(_) => {
                        self.pending.ops.clear();
                        Ok(())
                    }
                    Err(e) => bail!(e),
                }
            }
            None => {
                unreachable!();
            }
        }
    }

    fn revert(&mut self) -> Result<()> {
        self.pending.ops = vec![];
        Ok(())
    }

    fn restart(
        &mut self, _immediate_write: bool, no_revert: bool,
    ) -> Result<()> {
        if !no_revert {
            self.revert()?;
        }
        Ok(())
    }
}

impl Drop for KvdbParityDbTransaction {
    fn drop(&mut self) {
        // No-op
    }
}

impl KeyValueDbTraitTransactional for KvdbParitydb {
    type TransactionType = KvdbParityDbTransaction;

    fn start_transaction(
        &self, _immediate_write: bool,
    ) -> Result<Self::TransactionType> {
        Ok(KvdbParityDbTransaction {
            pending: self.kvdb.transaction(),
            col: self.col,
        })
    }
}

impl DeltaDbTrait for KvdbParitydb {}

use super::super::{
    super::storage_db::{delta_db_manager::DeltaDbTrait, key_value_db::*},
    errors::*,
};
use db::KeyValueStore;
use kvdb::DBTransaction;
use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};
use random_crash::random_crash_if_enabled;
use std::{any::Any, sync::Arc};
