// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

pub struct DeltaDbManagerParitydb {
    delta_db_path: PathBuf,
    creation_mutex: Mutex<()>,
}

impl DeltaDbManagerParitydb {
    const DELTA_DB_PARITYDB_DIR_PREFIX: &'static str = "paritydb_";
    const DELTA_DB_COLUMNS: u32 = 1;

    pub fn new(delta_db_path: PathBuf) -> Result<DeltaDbManagerParitydb> {
        if !delta_db_path.exists() {
            fs::create_dir_all(delta_db_path.clone())?;
        }

        Ok(Self {
            delta_db_path,
            creation_mutex: Default::default(),
        })
    }
    fn open_or_create_db(&self, delta_db_name: &str) -> Result<KvdbParitydb> {
        let path = self.get_delta_db_path(delta_db_name);
        let parity_config = db::ParityDbOpenConfig {
            columns: Self::DELTA_DB_COLUMNS,
            compression: None,
            disable_wal: false,
            stats: false,
        };
        let settings = db::paritydb_settings(path.clone(), &parity_config)?;
        let db = db::open_database(&settings)?;
        Ok(KvdbParitydb {
            kvdb: db.key_value(),
            col: 0,
        })
    }
}

impl DeltaDbManagerTrait for DeltaDbManagerParitydb {
    type DeltaDb = KvdbParitydb;

    fn get_delta_db_dir(&self) -> &Path {
        self.delta_db_path.as_path()
    }

    fn get_delta_db_name(&self, snapshot_epoch_id: &EpochId) -> String {
        Self::DELTA_DB_PARITYDB_DIR_PREFIX.to_string()
            + &snapshot_epoch_id.as_ref().to_hex::<String>()
    }

    fn get_delta_db_path(&self, delta_db_name: &str) -> PathBuf {
        self.delta_db_path.join(delta_db_name)
    }

    fn new_empty_delta_db(&self, delta_db_name: &str) -> Result<Self::DeltaDb> {
        let _lock = self.creation_mutex.lock();

        let path = self.get_delta_db_path(delta_db_name);
        if path.exists() {
            Err(ErrorKind::DeltaMPTAlreadyExists.into())
        } else {
            Ok(self.open_or_create_db(delta_db_name)?)
        }
    }

    fn get_delta_db(
        &self, delta_db_name: &str,
    ) -> Result<Option<Self::DeltaDb>> {
        let path = self.get_delta_db_path(delta_db_name);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(self.open_or_create_db(delta_db_name)?))
    }

    fn destroy_delta_db(&self, delta_db_name: &str) -> Result<()> {
        Ok(fs::remove_dir_all(self.get_delta_db_path(delta_db_name))?)
    }
}

use super::{
    super::{
        super::storage_db::delta_db_manager::DeltaDbManagerTrait, errors::*,
    },
    kvdb_paritydb::KvdbParitydb,
};
use parking_lot::Mutex;
use primitives::EpochId;
use rustc_hex::ToHex;
use std::{
    fs,
    path::{Path, PathBuf},
};
