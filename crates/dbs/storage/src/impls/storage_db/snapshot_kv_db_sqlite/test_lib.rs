// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

#[cfg(test)]
pub fn open_snapshot_db_for_testing(
    snapshot_path: &Path, readonly: bool, mpt_snapshot_path: &Path,
) -> Result<SnapshotDbSqlite> {
    use crate::impls::storage_db::snapshot_mpt_db_sqlite::SnapshotMptDbSqlite;

    use super::SnapshotKvDbSqlite;

    let mpt_snapshot = Arc::new(SnapshotMptDbSqlite::open(
        mpt_snapshot_path,
        readonly,
        &Default::default(),
        &Arc::new(Semaphore::new(DEFAULT_MAX_OPEN_SNAPSHOTS as usize)),
        None,
    )?);

    let kv_snapshot = SnapshotKvDbSqlite::open(
        snapshot_path,
        readonly,
        &Default::default(),
        &Arc::new(Semaphore::new(DEFAULT_MAX_OPEN_SNAPSHOTS as usize)),
    )?;

    Ok(SnapshotDbSqlite {
        snapshot_db: Arc::new(kv_snapshot),
        mpt_snapshot_db: Some(mpt_snapshot),
    })
}

#[cfg(test)]
use crate::impls::{errors::*, storage_db::snapshot_db_sqlite::SnapshotDbSqlite};

#[cfg(test)]
use crate::impls::{
    defaults::DEFAULT_MAX_OPEN_SNAPSHOTS,
    storage_db::snapshot_kv_db_sqlite::SnapshotDbTrait,
};
#[cfg(test)]
use std::{path::Path, sync::Arc};
#[cfg(test)]
use tokio::sync::Semaphore;
