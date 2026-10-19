// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

// Storage backend implementations — MDBX-native.
//
// Phase 5e completed the cutover: ParityDB is entirely gone from
// the storage crate. The hot tier (state, delta MPTs, snapshot
// info) lives in `mdbx` under `storage_db/`; the snapshot tier
// (KV / MPT / delta dumps) lives in its own dedicated
// `mdbx_snapshot` env per design doc §2.3.0.
//
// See `docs/storage-architecture.md` for the full column /
// prefix layout, and
// `docs/internal/storage-phase-5-cde-migration.md` for the
// migration story.

pub mod delta_db_manager_mdbx;
pub mod kvdb_mdbx;
pub mod mdbx_columns;
pub mod prefixed_kvdb_mdbx;
pub mod snapshot_db_manager_mdbx;
pub mod snapshot_debug;
pub mod snapshot_kv_db_mdbx;
pub mod snapshot_mpt;
pub mod snapshot_prefix;
