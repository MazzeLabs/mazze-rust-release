// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

mod config;
mod map;
mod node;
mod search;
mod update;

#[cfg(test)]
mod tests;

pub use self::{
    config::{
        ConsoliableWeight, Direction, KeyMngTrait, NoWeight,
        SharedKeyTreapMapConfig, TreapMapConfig,
    },
    map::{Iter, TreapMap},
    node::Node,
    search::{accumulate_weight_search, SearchDirection, SearchResult},
    update::ApplyOpOutcome,
};
