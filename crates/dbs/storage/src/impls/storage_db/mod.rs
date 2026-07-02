// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

// Storage backend implementations.
//
// Two-tier model — see [docs/storage-architecture.md](../../../../docs/storage-architecture.md):
//   - **ParityDB** (cold tier): blocks, receipts, traces, snapshot tries,
//     delta-MPT increments. Sole backend today for everything.
//   - **MDBX** (hot tier): live state for revm + recent DAG topology.
//     The `kvdb_mdbx` module is a stub awaiting Phase B implementation.
//
// The historical SQLite parallel implementation was removed because it
// was production-dead (only test/benchmark consumers, no `StateManager`
// wiring). MDBX replaces its role as "the second backend" — but at the
// hot tier rather than as a like-for-like ParityDB alternative.

pub mod delta_db_manager_paritydb;
pub mod kvdb_mdbx;
pub mod kvdb_paritydb;
pub mod mdbx_columns;
pub mod snapshot_db_manager_paritydb;
pub mod snapshot_debug;
pub mod snapshot_kv_db_paritydb;
pub mod snapshot_mpt;
