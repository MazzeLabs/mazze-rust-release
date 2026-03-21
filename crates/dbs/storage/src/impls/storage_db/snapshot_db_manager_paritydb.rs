// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

pub struct SnapshotDbManagerParitydb {
    snapshot_path: PathBuf,
    open_snapshot_semaphore: Arc<Semaphore>,
    already_open_snapshots: AlreadyOpenSnapshots<SnapshotKvDbParitydb>,
    mpt_snapshot_path: PathBuf,
    latest_snapshot_id: RwLock<(EpochId, u64)>,
    snapshot_epoch_id_before_recovered: RwLock<Option<EpochId>>,
    reconstruct_snapshot_id_for_reboot: RwLock<Option<EpochId>>,
}

impl SnapshotDbManagerParitydb {
    const SNAPSHOT_DB_PARITYDB_DIR_PREFIX: &'static str = "paritydb_";
    const MPT_SNAPSHOT_DIR: &'static str = "mpt_snapshot";

    pub fn new(
        snapshot_path: PathBuf, max_open_snapshots: u16,
        _use_isolated_db_for_mpt_table: bool,
        _use_isolated_db_for_mpt_table_height: Option<u64>,
        _era_epoch_count: u64,
    ) -> Result<Self> {
        if !snapshot_path.exists() {
            fs::create_dir_all(snapshot_path.clone())?;
        }

        let mpt_snapshot_path = snapshot_path
            .parent()
            .unwrap_or(&snapshot_path)
            .join(Self::MPT_SNAPSHOT_DIR);
        if !mpt_snapshot_path.exists() {
            fs::create_dir_all(&mpt_snapshot_path)?;
        }

        Ok(Self {
            snapshot_path,
            open_snapshot_semaphore: Arc::new(Semaphore::new(
                max_open_snapshots as usize,
            )),
            already_open_snapshots: Default::default(),
            mpt_snapshot_path,
            latest_snapshot_id: RwLock::new((NULL_EPOCH, 0)),
            snapshot_epoch_id_before_recovered: RwLock::new(None),
            reconstruct_snapshot_id_for_reboot: RwLock::new(None),
        })
    }

    pub fn update_latest_snapshot_id(&self, snapshot_id: EpochId, height: u64) {
        *self.latest_snapshot_id.write() = (snapshot_id, height);
    }

    pub fn clean_snapshot_epoch_id_before_recovered(&self) {
        *self.snapshot_epoch_id_before_recovered.write() = None;
    }

    pub fn set_reconstruct_snapshot_id(
        &self, reconstruct_main: Option<EpochId>,
    ) {
        debug!("set_reconstruct_snapshot_id to {:?}", reconstruct_main);
        *self.reconstruct_snapshot_id_for_reboot.write() = reconstruct_main;
    }

    pub fn recreate_latest_mpt_snapshot(&self) -> Result<()> {
        info!("recreate latest mpt snapshot (paritydb no-op)");
        Ok(())
    }

    fn acquire_open_permit(&self, try_open: bool) -> Result<()> {
        if try_open {
            self.open_snapshot_semaphore
                .try_acquire()
                .map_err(|_err| ErrorKind::SemaphoreTryAcquireError)?
                .forget();
        } else {
            futures::executor::block_on(self.open_snapshot_semaphore.acquire())
                .forget();
        }
        Ok(())
    }

    fn open_snapshot_readonly(
        &self, snapshot_path: PathBuf, try_open: bool,
    ) -> Result<Option<SnapshotKvDbParitydb>> {
        if !snapshot_path.exists() {
            return Ok(None);
        }

        self.acquire_open_permit(try_open)?;
        match SnapshotKvDbParitydb::open(
            &snapshot_path,
            true,
            &self.already_open_snapshots,
            &self.open_snapshot_semaphore,
        ) {
            Ok(snapshot_db) => Ok(Some(snapshot_db)),
            Err(e) => {
                self.open_snapshot_semaphore.add_permits(1);
                Err(e)
            }
        }
    }

    fn open_snapshot_write(
        &self, snapshot_path: PathBuf, create: bool,
    ) -> Result<SnapshotKvDbParitydb> {
        self.acquire_open_permit(false)?;
        let open_result = if create {
            SnapshotKvDbParitydb::create(
                &snapshot_path,
                &self.already_open_snapshots,
                &self.open_snapshot_semaphore,
                true,
            )
        } else {
            SnapshotKvDbParitydb::open(
                &snapshot_path,
                false,
                &self.already_open_snapshots,
                &self.open_snapshot_semaphore,
            )
        };

        match open_result {
            Ok(db) => Ok(db),
            Err(e) => {
                self.open_snapshot_semaphore.add_permits(1);
                Err(e)
            }
        }
    }

    fn get_merge_temp_snapshot_db_path(
        &self, old_snapshot_epoch_id: &EpochId, new_snapshot_epoch_id: &EpochId,
    ) -> PathBuf {
        self.snapshot_path.join(
            Self::SNAPSHOT_DB_PARITYDB_DIR_PREFIX.to_string()
                + "merge_temp_"
                + &old_snapshot_epoch_id.as_ref().to_hex::<String>()
                + &new_snapshot_epoch_id.as_ref().to_hex::<String>(),
        )
    }

    fn get_full_sync_temp_snapshot_db_path(
        &self, snapshot_epoch_id: &EpochId, merkle_root: &MerkleHash,
    ) -> PathBuf {
        self.snapshot_path.join(
            Self::SNAPSHOT_DB_PARITYDB_DIR_PREFIX.to_string()
                + "full_sync_temp_"
                + &snapshot_epoch_id.as_ref().to_hex::<String>()
                + &merkle_root.as_ref().to_hex::<String>(),
        )
    }

    fn rename_snapshot_db(from: &Path, to: &Path) -> Result<()> {
        if to.exists() {
            fs::remove_dir_all(to)?;
        }
        fs::rename(from, to).map_err(|e| e.into())
    }
}

impl SnapshotDbManagerTrait for SnapshotDbManagerParitydb {
    type SnapshotDb = SnapshotKvDbParitydb;
    type SnapshotDbWrite = SnapshotKvDbParitydb;

    fn get_snapshot_dir(&self) -> &Path {
        self.snapshot_path.as_path()
    }

    fn get_snapshot_db_name(&self, snapshot_epoch_id: &EpochId) -> String {
        Self::SNAPSHOT_DB_PARITYDB_DIR_PREFIX.to_string()
            + &snapshot_epoch_id.as_ref().to_hex::<String>()
    }

    fn get_snapshot_db_path(&self, snapshot_epoch_id: &EpochId) -> PathBuf {
        self.snapshot_path
            .join(self.get_snapshot_db_name(snapshot_epoch_id))
    }

    fn get_mpt_snapshot_dir(&self) -> &Path {
        self.mpt_snapshot_path.as_path()
    }

    fn get_latest_mpt_snapshot_db_name(&self) -> String {
        Self::SNAPSHOT_DB_PARITYDB_DIR_PREFIX.to_string() + "latest"
    }

    fn recovery_latest_mpt_snapshot_from_checkpoint(
        &self, _snapshot_epoch_id: &EpochId,
        _before_era_main_hash: Option<EpochId>,
    ) -> Result<()> {
        Ok(())
    }

    fn create_mpt_snapshot_from_latest(
        &self, _new_snapshot_epoch_id: &EpochId,
    ) -> Result<()> {
        Ok(())
    }

    fn get_epoch_id_from_snapshot_db_name(
        &self, snapshot_db_name: &str,
    ) -> Result<EpochId> {
        let prefix_len = Self::SNAPSHOT_DB_PARITYDB_DIR_PREFIX.len();
        Ok(EpochId::from_str(&snapshot_db_name[prefix_len..])
            .map_err(|_op| "not correct snapshot db name")?)
    }

    fn try_get_new_snapshot_epoch_from_temp_path(
        &self, dir_name: &str,
    ) -> Option<EpochId> {
        let prefix =
            Self::SNAPSHOT_DB_PARITYDB_DIR_PREFIX.to_string() + "merge_temp_";

        if dir_name.starts_with(&prefix) {
            match EpochId::from_str(
                &dir_name[(prefix.len() + EpochId::len_bytes() * 2)..],
            ) {
                Ok(e) => Some(e),
                Err(e) => {
                    error!(
                        "get new snapshot epoch id from temp path failed: {}",
                        e
                    );
                    None
                }
            }
        } else {
            None
        }
    }

    fn try_get_new_snapshot_epoch_from_mpt_temp_path(
        &self, _dir_name: &str,
    ) -> Option<EpochId> {
        None
    }

    fn new_snapshot_by_merging<'m>(
        &self, old_snapshot_epoch_id: &EpochId, snapshot_epoch_id: EpochId,
        delta_mpt: DeltaMptIterator,
        mut in_progress_snapshot_info: SnapshotInfo,
        snapshot_info_map_rwlock: &'m RwLock<PersistedSnapshotInfoMap>,
        _new_epoch_height: u64, recover_mpt_with_kv_snapshot_exist: bool,
    ) -> Result<(RwLockWriteGuard<'m, PersistedSnapshotInfoMap>, SnapshotInfo)>
    {
        info!(
            "new_snapshot_by_merging: old={:?} new={:?}",
            old_snapshot_epoch_id, snapshot_epoch_id,
        );

        if recover_mpt_with_kv_snapshot_exist {
            warn!(
                "recover_mpt_with_kv_snapshot_exist is not supported in paritydb snapshots"
            );
        }

        let in_reconstruct_snapshot_state = self
            .reconstruct_snapshot_id_for_reboot
            .write()
            .take()
            .is_some_and(|v| v == snapshot_epoch_id);

        let temp_db_path = self.get_merge_temp_snapshot_db_path(
            old_snapshot_epoch_id,
            &snapshot_epoch_id,
        );
        let new_snapshot_db_path =
            self.get_snapshot_db_path(&snapshot_epoch_id);

        let mut snapshot_kv_db =
            self.open_snapshot_write(temp_db_path.clone(), true)?;
        snapshot_kv_db.dump_delta_mpt(&delta_mpt)?;

        let new_snapshot_root = if *old_snapshot_epoch_id == NULL_EPOCH {
            snapshot_kv_db.direct_merge(
                None,
                &mut None,
                recover_mpt_with_kv_snapshot_exist,
                in_reconstruct_snapshot_state,
            )?
        } else {
            let old_snapshot = self
                .open_snapshot_readonly(
                    self.get_snapshot_db_path(old_snapshot_epoch_id),
                    false,
                )?
                .ok_or(Error::from(ErrorKind::SnapshotNotFound))?;
            snapshot_kv_db.copy_and_merge(
                &Arc::new(old_snapshot),
                &mut None,
                in_reconstruct_snapshot_state,
            )?
        };

        in_progress_snapshot_info.merkle_root = new_snapshot_root.clone();
        drop(snapshot_kv_db);

        let locked = snapshot_info_map_rwlock.write();
        Self::rename_snapshot_db(&temp_db_path, &new_snapshot_db_path)?;

        Ok((locked, in_progress_snapshot_info))
    }

    fn get_snapshot_by_epoch_id(
        &self, snapshot_epoch_id: &EpochId, try_open: bool,
        _open_mpt_snapshot: bool,
    ) -> Result<Option<Self::SnapshotDb>> {
        if snapshot_epoch_id.eq(&NULL_EPOCH) {
            return Ok(Some(Self::SnapshotDb::get_null_snapshot()));
        }
        let path = self.get_snapshot_db_path(snapshot_epoch_id);
        self.open_snapshot_readonly(path, try_open)
    }

    fn destroy_snapshot(&self, snapshot_epoch_id: &EpochId) -> Result<()> {
        Ok(fs::remove_dir_all(
            self.get_snapshot_db_path(snapshot_epoch_id),
        )?)
    }

    fn new_temp_snapshot_for_full_sync(
        &self, snapshot_epoch_id: &EpochId, merkle_root: &MerkleHash,
        _new_epoch_height: u64,
    ) -> Result<Self::SnapshotDbWrite> {
        let temp_db_path = self.get_full_sync_temp_snapshot_db_path(
            snapshot_epoch_id,
            merkle_root,
        );
        self.open_snapshot_write(temp_db_path, true)
    }

    fn finalize_full_sync_snapshot<'m>(
        &self, snapshot_epoch_id: &EpochId, merkle_root: &MerkleHash,
        snapshot_info_map_rwlock: &'m RwLock<PersistedSnapshotInfoMap>,
    ) -> Result<RwLockWriteGuard<'m, PersistedSnapshotInfoMap>> {
        let temp_db_path = self.get_full_sync_temp_snapshot_db_path(
            snapshot_epoch_id,
            merkle_root,
        );
        let final_db_path = self.get_snapshot_db_path(snapshot_epoch_id);
        let locked = snapshot_info_map_rwlock.write();
        Self::rename_snapshot_db(&temp_db_path, &final_db_path)?;
        Ok(locked)
    }
}

use crate::{
    impls::{
        delta_mpt::DeltaMptIterator, errors::*,
        storage_manager::PersistedSnapshotInfoMap,
    },
    storage_db::{
        AlreadyOpenSnapshots, SnapshotDbManagerTrait, SnapshotDbTrait,
        SnapshotInfo,
    },
};
use parking_lot::{RwLock, RwLockWriteGuard};
use primitives::{EpochId, MerkleHash, NULL_EPOCH};
use rustc_hex::ToHex;
use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};
use tokio::sync::Semaphore;

use super::snapshot_kv_db_paritydb::SnapshotKvDbParitydb;
