// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

#[macro_use]
extern crate log;

mod evm;
#[macro_use]
pub mod factory;
pub mod instructions;
mod interpreter;
mod vmtype;
#[macro_use]
extern crate lazy_static;

#[cfg(test)]
mod tests;

pub use self::{
    evm::{CostType, FinalizationResult, Finalize},
    factory::Factory,
    instructions::{GasPriceTier, INSTRUCTIONS, INSTRUCTIONS_CANCUN},
    vmtype::VMType,
};
