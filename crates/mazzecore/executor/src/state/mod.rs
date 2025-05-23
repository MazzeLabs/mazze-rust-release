// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Ledger State: Acts as a caching and checkpoint layer built upon semantically
//! meaningful database interfaces for the execution.

/// Global Statistic Variables: Manages global variables with different
/// checkpoint and caching mechanisms compared to other state variables, such
/// as, `total_issued_tokens`.
mod global_stat;

/// Overlay Account: Defines the access and manipulation object during
/// execution. Each `OverlayAccount` encompasses both the database-stored
/// information of an account and its in-execution data.
mod overlay_account;

/// State Object: Represents the core object of the state module.
mod state_object;

#[cfg(test)]
pub use state_object::get_state_for_genesis_write;
pub use state_object::{
    settle_collateral_for_all, State, StateCommitResult,
    COMMISSION_PRIVILEGE_SPECIAL_KEY,
};

use mazze_types::AddressWithSpace;
use std::collections::HashSet;

/// Mode of dealing with null accounts.
#[derive(PartialEq)]
pub enum CleanupMode<'a> {
    /// Create accounts which would be null.
    ForceCreate,
    /// Don't delete null accounts upon touching, but also don't create them.
    NoEmpty,
    /// Mark all touched accounts.
    /// TODO: We have not implemented the correct behavior of TrackTouched for
    /// internal Contracts.
    TrackTouched(&'a mut HashSet<AddressWithSpace>),
}
