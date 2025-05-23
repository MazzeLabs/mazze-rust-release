// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

// Recursion limit raised for error_chain
#![recursion_limit = "256"]

extern crate mazze_bytes as bytes;
#[macro_use]
extern crate mazze_internal_common;
extern crate keccak_hash as hash;
extern crate mazzekey as keylib;
#[macro_use]
extern crate log;
#[macro_use]
extern crate error_chain;
extern crate db as ext_db;
#[macro_use]
extern crate lazy_static;
extern crate static_assertions;
extern crate sha3_macro;
extern crate substrate_bn as bn;

#[macro_use]
pub mod message;

pub mod block_data_manager;
pub mod cache_config;
pub mod cache_manager;
pub mod channel;
pub mod client;
pub mod consensus;
pub mod db;
pub mod error;
pub mod genesis_block;
pub mod light_protocol;
pub mod node_type;
pub mod pow;
pub mod rpc_errors;
pub mod state_exposer;
mod state_prefetcher;
pub mod statistics;
pub mod sync;
pub mod transaction_pool;
pub mod unique_id;
pub mod verification;

pub use crate::{
    block_data_manager::BlockDataManager,
    channel::Notifications,
    consensus::{
        BestInformation, ConsensusGraph, ConsensusGraphTrait,
        SharedConsensusGraph,
    },
    light_protocol::{
        Handler as LightHandler, Provider as LightProvider,
        QueryService as LightQueryService,
    },
    node_type::NodeType,
    sync::{
        SharedSynchronizationGraph, SharedSynchronizationService,
        SynchronizationGraph, SynchronizationService,
    },
    transaction_pool::{SharedTransactionPool, TransactionPool},
    unique_id::UniqueId,
};
pub use mazze_parameters::{
    block as block_parameters, consensus as consensus_parameters,
    consensus_internal as consensus_internal_parameters,
    sync as sync_parameters, WORKER_COMPUTATION_PARALLELISM,
};
pub use network::PeerInfo;

pub trait Stopable {
    fn stop(&self);
}
