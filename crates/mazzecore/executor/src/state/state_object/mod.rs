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

/// Implements access functions global statistic variables of `State`.
mod global_statistics;

/// Implements functions for the sponsorship mechanism of `State`.
mod sponsor;

/// Implements access functions for the account storage entries of `State`.
mod storage_entry;

/// Implements functions for the base fee proportion in the state.
mod reward;

#[cfg(test)]
mod tests;

pub use self::{
    collateral::{set_initial_storage_point_prop, settle_collateral_for_all},
    commit::StateCommitResult,
    reward::set_initial_base_fee_prop,
    sponsor::COMMISSION_PRIVILEGE_SPECIAL_KEY,
};
#[cfg(test)]
pub use tests::get_state_for_genesis_write;

use self::checkpoints::CheckpointLayer;
use super::{
    global_stat::GlobalStat,
    overlay_account::{AccountEntry, OverlayAccount, RequireFields},
};
use crate::substate::Substate;
use mazze_statedb::{
    Result as DbResult, StateDbExt, StateDbGeneric as StateDb,
};
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

    /// In-memory global statistic variables.
    // TODO: try not to make it special?
    global_stat: GlobalStat,

    /// Checkpoint layers for the account entries
    checkpoints: RwLock<Vec<CheckpointLayer>>,
}

impl State {
    pub fn new(db: StateDb) -> DbResult<Self> {
        let initialized = db.is_initialized()?;

        let world_stat = if initialized {
            GlobalStat::loaded(&db)?
        } else {
            GlobalStat::assert_non_inited(&db)?;
            GlobalStat::new()
        };

        Ok(State {
            db,
            cache: Default::default(),
            checkpoints: Default::default(),
            global_stat: world_stat,
        })
    }

    pub fn prefetch_account(&self, address: &AddressWithSpace) -> DbResult<()> {
        self.prefetch(address, RequireFields::Code)
    }

    pub fn set_initial_storage_point_prop(&mut self) -> DbResult<()> {
        set_initial_storage_point_prop(self)
    }

    pub fn set_initial_base_fee_prop(&mut self) {
        set_initial_base_fee_prop(self)
    }
}
