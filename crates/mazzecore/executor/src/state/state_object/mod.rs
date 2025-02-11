//! A caching and checkpoint layer built upon semantically meaningful database
//! interfaces, providing interfaces and logics for managing accounts and global
//! statistics to the execution engine.

/// Contract Manager: Responsible for creating and deleting contract objects.
mod contract_manager;

/// Implements access functions for the basic fields (e.g., balance, nonce) of
/// `State`.
mod basic_fields;

/// Cache Layer: Implements a read-through write-back cache logic and provides
/// interfaces for reading and writing account data. It also handles the logic
/// for loading extension fields of an account.
mod cache_layer;

/// Checkpoints: Defines the account entry type within checkpoint layers and
/// implements checkpoint maintenance logic.
mod checkpoints;

/// Implements functions for the storage collateral of `State`.
mod collateral;

/// Implements functions for committing `State` changes to db.
mod commit;

/// Implements functions for the sponsorship mechanism of `State`.
mod sponsor;

/// Implements access functions for the account storage entries of `State`.
mod storage_entry;

mod reward;

#[cfg(test)]
mod tests;

pub use self::{
    commit::StateCommitResult, reward::initialize_cip137,
    sponsor::COMMISSION_PRIVILEGE_SPECIAL_KEY,
};
#[cfg(test)]
pub use tests::get_state_for_genesis_write;

use self::checkpoints::CheckpointLayer;
use super::overlay_account::{AccountEntry, OverlayAccount, RequireFields};
use crate::substate::Substate;
use mazze_statedb::{Result as DbResult, StateDbGeneric as StateDb};
use mazze_types::AddressWithSpace;
use parking_lot::RwLock;
use std::collections::HashMap;

/// A caching and checkpoint layer built upon semantically meaningful database
/// interfaces, providing interfaces and logics for managing accounts and global
/// statistics to the execution engine.
pub struct State {
    /// The backend database
    pub(super) db: StateDb,

    /// Caches for the account entries
    ///
    /// WARNING: Don't delete cache entries outside of `State::commit`, unless
    /// you are familiar with checkpoint maintenance.
    cache: RwLock<HashMap<AddressWithSpace, AccountEntry>>,

    /// Checkpoint layers for the account entries
    checkpoints: RwLock<Vec<CheckpointLayer>>,
}

impl State {
    pub fn new(db: StateDb) -> DbResult<Self> {
        Ok(State {
            db,
            cache: Default::default(),
            checkpoints: Default::default(),
        })
    }

    pub fn prefetch_account(&self, address: &AddressWithSpace) -> DbResult<()> {
        self.prefetch(address, RequireFields::Code)
    }
}
