// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

#[macro_use]
extern crate lazy_static;

pub mod genesis;
pub mod internal_contract_addresses;

pub mod consensus {
    pub const DEFERRED_STATE_EPOCH_COUNT: u64 = 5;
    pub const EPOCH_SET_PERSISTENCE_DELAY: u64 = DEFERRED_STATE_EPOCH_COUNT;

    pub const ADAPTIVE_WEIGHT_DEFAULT_BETA: u64 = 1000;
    pub const HEAVY_BLOCK_DEFAULT_DIFFICULTY_RATIO: u64 = 250;
    pub const TIMER_CHAIN_BLOCK_DEFAULT_DIFFICULTY_RATIO: u64 = 180;
    pub const TIMER_CHAIN_DEFAULT_BETA: u64 = 240;
    // The number of epochs per era. Each era is a potential checkpoint
    // position. The parent_edge checking and adaptive checking are defined
    // relative to the era start blocks.
    pub const ERA_DEFAULT_EPOCH_COUNT: u64 = 20000;

    // At Mazze MainNet Launch there are approximately 2 blocks per epoch,
    // with 1k TPS, and 2 blocks per second, a DeltaMPT contains data for
    // around 2 million transaction.
    pub const SNAPSHOT_EPOCHS_CAPACITY: u32 = 2000;

    pub const NULL: usize = !0;
    pub const NULLU64: u64 = !0;

    pub const MAX_BLAME_RATIO_FOR_TRUST: f64 = 0.4;

    pub const TRANSACTION_DEFAULT_EPOCH_BOUND: u64 = 100000;

    pub const GENESIS_GAS_LIMIT: u64 = 30_000_000;

    pub const ONE_MAZZE_IN_MAZZY: u64 = 1_000_000_000_000_000_000;

    pub const ONE_UMAZZE_IN_MAZZY: u64 = 1_000_000_000_000;

    pub const ONE_MAZZE_IN_UMAZZE: u64 =
        ONE_MAZZE_IN_MAZZY / ONE_UMAZZE_IN_MAZZY;

    pub const ONE_GMAZZY_IN_MAZZY: u64 = 1_000_000_000;

    pub const NEXT_HARDFORK_HEADER_CUSTOM_FIRST_ELEMENT: [u8; 1] = [1];
}

pub mod consensus_internal {
    use crate::consensus::{ONE_GMAZZY_IN_MAZZY, ONE_MAZZE_IN_MAZZY};

    pub const OUTLIER_PENALTY_UPPER_EPOCH_COUNT: u64 = 10;
    pub const OUTLIER_PENALTY_RATIO: u64 = 100;
    /// The maximum number of blocks to be executed in each epoch
    pub const EPOCH_EXECUTED_BLOCK_BOUND: usize = 200;
    // The initial base mining reward in uMAZZE.
    pub const INITIAL_BASE_MINING_REWARD_IN_UMAZZE: u64 = 4_000_000;

    pub const GENESIS_TOKEN_COUNT_IN_MAZZE: u64 = 2_500_000_000;
    pub const MAX_SUPPLY_TOKEN_COUNT_IN_MAZZE: u64 = 5_000_000_000;
    pub const HALVING_INTERVAL_IN_BLOCKS: u64 = 312_500_000;

    /// This is the cap of the size of the outlier barrier. If we have more
    /// than this number we will use the brute_force O(n) algorithm instead.
    pub const OUTLIER_BARRIER_CAP: usize = 100;
    /// Here is the delay for us to recycle those orphaned blocks in the
    /// boundary of eras and large epochs.
    pub const RECYCLE_TRANSACTION_DELAY: u64 = 20;
    /// This is the cap of the size of `blockset_in_own_view_of_epoch`. If we
    /// have more than this number, we will not store it in memory
    pub const BLOCKSET_IN_OWN_VIEW_OF_EPOCH_CAP: u64 = 1000;

    /// This is the minimum risk that the confirmation meter tries to maintain.
    pub const CONFIRMATION_METER_MIN_MAINTAINED_RISK: f64 = 0.00000001;
    /// The maximum number of epochs that the confirmation meter tries to
    /// maintain internally.
    pub const CONFIRMATION_METER_MAX_NUM_MAINTAINED_RISK: usize = 100;
    /// The minimum timer diff value for the adaptive test in confirmation meter
    /// to consider
    pub const CONFIRMATION_METER_ADAPTIVE_TEST_TIMER_DIFF: u64 = 140;
    /// The batch step in the confirmation meter to do the adaptive test
    pub const CONFIRMATION_METER_PSI: u64 = 30;
    /// The maximum value of adaptive block generation risk that a confirmation
    /// meter is going to consider safe to assume no adaptive blocks in the
    /// near future.
    pub const CONFIRMATION_METER_MAXIMUM_ADAPTIVE_RISK: f64 = 0.0000001;
    /// This controls how often the confirmation meter updates. The default is
    /// to update the meter every 20 blocks. Note that confirmation meter
    /// update is CPU intensive if the tree graph is in a unstable state.
    pub const CONFIRMATION_METER_UPDATE_FREQUENCY: usize = 20;

    /// The storage point proportion
    pub const STORAGE_POINT_PROP_INIT: u64 = ONE_MAZZE_IN_MAZZY;

    /// The initial base price share proportion
    pub const BASEFEE_PROP_INIT: u64 = ONE_MAZZE_IN_MAZZY;

    /// The initial and minimum base price
    pub const INITIAL_1559_CORE_BASE_PRICE: u64 = ONE_GMAZZY_IN_MAZZY;

    pub const INITIAL_1559_ETH_BASE_PRICE: u64 = 20 * ONE_GMAZZY_IN_MAZZY;

    // Parameter specified in EIP-1559
    pub const ELASTICITY_MULTIPLIER: usize = 2;
}

pub mod rpc {
    pub const GAS_PRICE_BLOCK_SAMPLE_SIZE: usize = 100;
    pub const EVM_GAS_PRICE_BLOCK_SAMPLE_SIZE: usize = 20;
    pub const GAS_PRICE_TRANSACTION_SAMPLE_SIZE: usize = 10000;
    pub const EVM_GAS_PRICE_TRANSACTION_SAMPLE_SIZE: usize = 1000;
    pub const TRANSACTION_COUNT_PER_BLOCK_WATER_LINE_LOW: usize = 100;
    pub const TRANSACTION_COUNT_PER_BLOCK_WATER_LINE_MEDIUM: usize = 600;
    pub const GAS_PRICE_DEFAULT_VALUE: usize = 1_000_000_000;
}

pub mod sync {
    use std::time::Duration;

    /// The threshold controlling whether a node is in catch-up mode.
    /// A node is in catch-up mode if its local best epoch number is
    /// CATCH_UP_EPOCH_LAG_THRESHOLD behind the median of the epoch
    /// numbers of peers.
    pub const CATCH_UP_EPOCH_LAG_THRESHOLD: u64 = 20;
    /// This threshold controlling whether a node should request missing
    /// terminals from peers when the node is in catch-up mode.
    pub const REQUEST_TERMINAL_EPOCH_LAG_THRESHOLD: u64 = 40;

    /// The max number of headers that are to be sent for header
    /// block request.
    pub const MAX_HEADERS_TO_SEND: u64 = 512;
    /// The max number of blocks that are to be sent for compact block request.
    pub const MAX_BLOCKS_TO_SEND: u64 = 128;
    /// The max number of epochs whose hashes are to be responded
    /// for request GetBlockHashesByEpoch
    pub const MAX_EPOCHS_TO_SEND: u64 = 128;
    pub const MAX_PACKET_SIZE: usize = 15 * 1024 * 1024 + 512 * 1024; // 15.5 MB

    /// The threshold controlling whether we should query local_block_info in
    /// disk when requesting block header or block. If the difference
    /// between height of the block and current best height is less than
    /// LOCAL_BLOCK_INFO_QUERY_THRESHOLD, we can request block directly through
    /// network, otherwise we should check disk first.
    pub const LOCAL_BLOCK_INFO_QUERY_THRESHOLD: u64 = 5;

    /// Measured block propagation delay in *seconds*. This will determine the
    /// conservative window when we measure confirmation risk internally in
    /// the consensus layer.
    pub const BLOCK_PROPAGATION_DELAY: u64 = 10;

    lazy_static! {
        // The waiting time duration that will be accumulated for resending a
        // timeout request.
        pub static ref REQUEST_START_WAITING_TIME: Duration =
            Duration::from_secs(1);

        // The waiting time duration before resending a request which failed
        // due to sending error.
        pub static ref FAILED_REQUEST_RESEND_WAIT: Duration =
            Duration::from_millis(50);
    }
    //const REQUEST_WAITING_TIME_BACKOFF: u32 = 2;
    pub const DEFAULT_CHUNK_SIZE: u64 = 256 * 1024;
}

pub mod pow {
    // This factor N controls the bound of each difficulty adjustment.
    // The new difficulty should be in the range of [(1-1/N)*D, (1+1/N)*D],
    // where D is the old difficulty.
    pub const DIFFICULTY_ADJUSTMENT_FACTOR: usize = 2;

    pub const DIFFICULTY_ADJUSTMENT_EPOCH_PERIOD: u64 = 25;
    // Time unit is micro-second (usec)
    // We target two blocks per second. This strikes a good balance between the
    // growth of the metadata, the memory consumption of the consensus graph,
    // and the confirmation speed
    // Current value is 0.250 seconds (250000 usec), lowered from 0.5 seconds (500000 usec)
    // This value is being used to compute the number of blocks per hour, day, year.
    // One second is 1000000 usec

    pub const ONE_SECOND_IN_USEC: u64 = 1000000;
    pub const TARGET_AVERAGE_BLOCK_GENERATION_PERIOD: u64 = 250000;

    // TODO: compute a more appropriate initial difficulty
    // previous initial difficulty: 20_000_000_000;
    pub const INITIAL_DIFFICULTY: u64 = 10;

    // The amount of epochs to use for switching mining seed hash
    pub const RANDOMX_EPOCH_LENGTH: u64 = 2048;
}

pub mod tx_pool {
    pub const TXPOOL_DEFAULT_NONCE_BITS: usize = 128;
}

pub mod block {
    use crate::consensus::GENESIS_GAS_LIMIT;

    // The maximum block size limit in bytes
    // Consider that the simple payment transaction consumes only 100 bytes per
    // second. This would allow us to have 2000 simple payment transactions
    // per block. With two blocks per second, we will have 4000TPS at the
    // peak with only simple payment, which is good enough for now.
    pub const MAX_BLOCK_SIZE_IN_BYTES: usize = 200 * 1024;
    // The maximum number of transactions to be packed in a block given
    // `MAX_BLOCK_SIZE_IN_BYTES`, assuming 50-byte transactions.
    pub const ESTIMATED_MAX_BLOCK_SIZE_IN_TRANSACTION_COUNT: usize = 4096;
    // The maximum number of referees allowed for each block
    pub const REFEREE_DEFAULT_BOUND: usize = 200;
    // The maximal length of custom data in block header
    pub const HEADER_CUSTOM_LENGTH_BOUND: usize = 64;
    // If a new block is more than valid_time_drift ahead of the current system
    // timestamp, it will be discarded (but may get received again) and the
    // peer will be disconnected.
    pub const VALID_TIME_DRIFT: u64 = 10 * 60;
    // A new block has to be less than this drift to send to the consensus
    // graph. Otherwise, it will be queued at the synchronization layer.
    pub const ACCEPTABLE_TIME_DRIFT: u64 = 5 * 60;
    // FIXME: a block generator parameter only. We should remove this later
    pub const MAX_TRANSACTION_COUNT_PER_BLOCK: usize = 20000;
    pub const DEFAULT_TARGET_BLOCK_GAS_LIMIT: u64 = GENESIS_GAS_LIMIT;
    // The following parameter controls how many blocks are allowed to
    // contain EVM Space transactions. Setting it to N means that one block
    // must has a height of the multiple of N to contain EVM transactions.
    pub const EVM_TRANSACTION_BLOCK_RATIO: u64 = 5;
    // The following parameter controls the ratio of gas limit allowed for
    // EVM space transactions. Setting it to N means that only 1/N of th
    // block gas limit can be used for EVM transaction enabled blocks.
    pub const EVM_TRANSACTION_GAS_RATIO: u64 = 2;
    // The following parameter controls the ratio of gas can be passed to EVM
    // space in the cross space call. Setting it to N means that only 1/N of gas
    // left can be passed to the cross space call.
    pub const CROSS_SPACE_GAS_RATIO: u64 = 10;
}

pub mod collateral {
    use crate::consensus::ONE_MAZZE_IN_MAZZY;
    use mazze_types::U256;

    /// This is the storage collateral units for each KiB of code, amount in
    /// COLLATERAL_UNITs. Code collateral is calculated by each whole KiB
    /// rounding upwards.
    pub const CODE_COLLATERAL_UNITS_PER_KI_BYTES: u64 = 512;
    /// This is the storage collateral units to deposit for one key/value pair
    /// in storage. 1 MAZZE for 16 key value entries.
    pub const COLLATERAL_UNITS_PER_STORAGE_KEY: u64 = 64;

    lazy_static! {
        /// This is the unit of storage collateral to deposit
        pub static ref MAZZIES_PER_STORAGE_COLLATERAL_UNIT: U256 =
            (ONE_MAZZE_IN_MAZZY / 1024).into();
        /// The collaterals in mazzies for one key/value pair in storage.
        pub static ref COLLATERAL_MAZZIES_PER_STORAGE_KEY: U256 =
            *MAZZIES_PER_STORAGE_COLLATERAL_UNIT
            * COLLATERAL_UNITS_PER_STORAGE_KEY;
    }

    pub fn code_collateral_units(len: usize) -> u64 {
        (len as u64 + 1023) / 1024 * CODE_COLLATERAL_UNITS_PER_KI_BYTES
    }
}

pub mod light {
    use std::time::Duration;

    lazy_static! {
        /// Frequency of re-triggering sync.
        pub static ref SYNC_PERIOD: Duration = Duration::from_secs(1);

        /// Frequency of checking request timeouts.
        pub static ref CLEANUP_PERIOD: Duration = Duration::from_secs(1);

        /// Frequency of sending StatusPing message to peers.
        pub static ref HEARTBEAT_PERIOD: Duration = Duration::from_secs(30);

        /// Request timeouts.
        pub static ref EPOCH_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
        pub static ref HEADER_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
        pub static ref WITNESS_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
        pub static ref BLOOM_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
        pub static ref RECEIPT_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
        pub static ref BLOCK_TX_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
        pub static ref STATE_ROOT_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
        pub static ref STATE_ENTRY_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
        pub static ref TX_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
        pub static ref TX_INFO_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
        pub static ref STORAGE_ROOT_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

        /// Maximum time period we wait for a response for an on-demand query.
        /// After this timeout has been reached, we try another peer or give up.
        pub static ref MAX_POLL_TIME: Duration = Duration::from_secs(4);

        /// Items not accessed for this amount of time are removed from the cache.
        pub static ref CACHE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
    }

    /// The threshold controlling whether a node is in catch-up mode.
    /// A node is in catch-up mode if its local best epoch number is
    /// `CATCH_UP_EPOCH_LAG_THRESHOLD` behind the median of the epoch
    /// numbers of peers.
    pub const CATCH_UP_EPOCH_LAG_THRESHOLD: u64 = 3;

    /// (Maximum) number of items requested in a single request.
    pub const EPOCH_REQUEST_BATCH_SIZE: usize = 100;
    pub const HEADER_REQUEST_BATCH_SIZE: usize = 30;
    pub const BLOOM_REQUEST_BATCH_SIZE: usize = 30;
    pub const WITNESS_REQUEST_BATCH_SIZE: usize = 50;
    pub const RECEIPT_REQUEST_BATCH_SIZE: usize = 30;
    pub const BLOCK_TX_REQUEST_BATCH_SIZE: usize = 30;
    pub const STATE_ROOT_REQUEST_BATCH_SIZE: usize = 30;
    pub const STATE_ENTRY_REQUEST_BATCH_SIZE: usize = 30;
    pub const TX_REQUEST_BATCH_SIZE: usize = 30;
    pub const TX_INFO_REQUEST_BATCH_SIZE: usize = 30;
    pub const STORAGE_ROOT_REQUEST_BATCH_SIZE: usize = 30;

    /// Maximum number of in-flight items at any given time.
    /// If we reach this limit, we will not request any more.
    pub const MAX_HEADERS_IN_FLIGHT: usize = 1000;
    pub const MAX_WITNESSES_IN_FLIGHT: usize = 500;
    pub const MAX_BLOOMS_IN_FLIGHT: usize = 500;
    pub const MAX_RECEIPTS_IN_FLIGHT: usize = 100;
    pub const MAX_BLOCK_TXS_IN_FLIGHT: usize = 100;
    pub const MAX_STATE_ROOTS_IN_FLIGHT: usize = 100;
    pub const MAX_STATE_ENTRIES_IN_FLIGHT: usize = 100;
    pub const MAX_TXS_IN_FLIGHT: usize = 100;
    pub const MAX_TX_INFOS_IN_FLIGHT: usize = 100;
    pub const MAX_STORAGE_ROOTS_IN_FLIGHT: usize = 100;

    /// Maximum number of in-flight epoch requests at any given time.
    /// Similar to `MAX_HEADERS_IN_FLIGHT`. However, it is hard to match
    /// hash responses to epoch requests, so we count the requests instead.
    pub const MAX_PARALLEL_EPOCH_REQUESTS: usize = 10;

    /// Number of epochs to request in one round (in possibly multiple batches).
    pub const NUM_EPOCHS_TO_REQUEST: usize = 200;

    /// Minimum number of missing items in the sync pipeline.
    /// If we have fewer, we will try to request some more.
    pub const NUM_WAITING_HEADERS_THRESHOLD: usize = 1000;

    /// Max number of epochs/headers/txs to send to a light peer in a response.
    pub const MAX_EPOCHS_TO_SEND: usize = 128;
    pub const MAX_HEADERS_TO_SEND: usize = 512;
    pub const MAX_TXS_TO_SEND: usize = 1024;
    pub const MAX_WITNESSES_TO_SEND: usize = 100;
    pub const MAX_ITEMS_TO_SEND: usize = 50;

    /// During syncing, we might transiently have enough malicious blaming
    /// blocks to consider a correct header incorrect. For this reason, we
    /// first wait for enough header to accumulate before checking blaming.
    /// TODO(thegaram): review value and expose this as a parameter
    pub const BLAME_CHECK_OFFSET: u64 = 20;

    /// During log filtering, we stream a set of items (blooms, receipts, txs)
    /// to match against. To make the process faster, we need to make sure that
    /// there's always plenty of items in flight. This way, we can reduce idle
    /// time when we're waiting to receive an item.
    pub const LOG_FILTERING_LOOKAHEAD: usize = 100;

    // Number of blocks to sample for mazze_gasPrice.
    pub const GAS_PRICE_BLOCK_SAMPLE_SIZE: usize = 30;

    // Maximum number of transactions to sample for mazze_gasPrice.
    pub const GAS_PRICE_TRANSACTION_SAMPLE_SIZE: usize = 1000;

    pub const TRANSACTION_COUNT_PER_BLOCK_WATER_LINE_LOW: usize = 100;
    pub const TRANSACTION_COUNT_PER_BLOCK_WATER_LINE_MEDIUM: usize = 600;

    // Number of blocks we retrieve in parallel for the gas price sample.
    pub const GAS_PRICE_BATCH_SIZE: usize = 30;
}

pub const WORKER_COMPUTATION_PARALLELISM: usize = 8;
