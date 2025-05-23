use super::OverlayAccount;
use keccak_hash::KECCAK_EMPTY;
use mazze_bytes::Bytes;
use mazze_statedb::{
    ErrorKind as DbErrorKind, Result as DbResult, StateDbExt, StateDbGeneric,
};
use mazze_types::Address;
use std::sync::Arc;

impl OverlayAccount {
    /// Check should load lazily maintained fields
    pub fn should_load_ext_fields(&self, require: RequireFields) -> bool {
        trace!("update_account_cache account={:?}", self);
        match require {
            RequireFields::None => false,
            RequireFields::Code => !self.is_code_loaded(),
        }
    }

    /// Load lazily maintained code
    pub fn cache_code(&mut self, db: &StateDbGeneric) -> DbResult<()> {
        trace!(
            "OverlayAccount::cache_code: ic={}; self.code_hash={:?}, self.code_cache={:?}",
               self.is_code_loaded(), self.code_hash, self.code);

        if self.is_code_loaded() {
            return Ok(());
        }

        self.code = db.get_code(&self.address, &self.code_hash)?;
        if self.code.is_none() {
            warn!(
                "Failed to get code {:?} for address {:?}",
                self.code_hash, self.address
            );

            bail!(DbErrorKind::IncompleteDatabase(self.address.address));
        }

        Ok(())
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum RequireFields {
    None,
    Code,
}

const NOT_LOADED_ERR: &'static str =
    "OverlayAccount Ext fields not loaded before read";

impl OverlayAccount {
    /// To prevent panics from reading ext fields without loading from the DB,
    /// these method are restricted to be visible only within the `state`
    /// module.
    pub(in crate::state) fn code_size(&self) -> usize {
        if self.code_hash == KECCAK_EMPTY {
            0
        } else {
            self.code.as_ref().expect(NOT_LOADED_ERR).code_size()
        }
    }

    /// To prevent panics from reading ext fields without loading from the DB,
    /// these method are restricted to be visible only within the `state`
    /// module.
    pub(in crate::state) fn code(&self) -> Option<Arc<Bytes>> {
        if self.code_hash == KECCAK_EMPTY {
            None
        } else {
            Some(self.code.as_ref().expect(NOT_LOADED_ERR).code.clone())
        }
    }

    /// To prevent panics from reading ext fields without loading from the DB,
    /// these method are restricted to be visible only within the `state`
    /// module.
    pub(in crate::state) fn code_owner(&self) -> Address {
        if self.code_hash == KECCAK_EMPTY {
            Address::zero()
        } else {
            self.code.as_ref().expect(NOT_LOADED_ERR).owner
        }
    }
}
