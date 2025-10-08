// Copyright 2015-2018 Parity Technologies (UK) Ltd.
// This file is part of Parity.

// Parity is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity.  If not, see <http://www.gnu.org/licenses/>.

// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::KeyValueStore;
use kvdb::{DBOp, DBTransaction, DBValue, IoStats, IoStatsKind, KeyValueDB};
use kvdb_rocksdb::{CompactionProfile, Database as RocksDatabase, DatabaseConfig as RocksConfig};
use parity_db::{CompressionType, Db as ParityDb, Options as ParityOptions};
use parity_util_mem::{MallocSizeOf, MallocSizeOfOps};
use std::{
    convert::TryInto,
    fs,
    io,
    iter,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

pub struct SystemDB {
    key_value: Arc<dyn KeyValueStore>,
}

impl SystemDB {
    pub fn key_value(&self) -> Arc<dyn KeyValueStore> {
        Arc::clone(&self.key_value)
    }

    pub fn new(kvdb: Arc<dyn KeyValueStore>) -> Self {
        Self { key_value: kvdb }
    }
}

/// db compaction profile
#[derive(Debug, PartialEq, Clone)]
pub enum DatabaseCompactionProfile {
    /// Try to determine compaction profile automatically
    Auto,
    /// SSD compaction profile
    SSD,
    /// HDD or other slow storage io compaction profile
    HDD,
}

impl Default for DatabaseCompactionProfile {
    fn default() -> Self {
        DatabaseCompactionProfile::Auto
    }
}

impl FromStr for DatabaseCompactionProfile {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto" => Ok(DatabaseCompactionProfile::Auto),
            "ssd" => Ok(DatabaseCompactionProfile::SSD),
            "hdd" => Ok(DatabaseCompactionProfile::HDD),
            _ => Err(
                "Invalid compaction profile given. Expected auto/hdd/ssd."
                    .into(),
            ),
        }
    }
}

#[derive(Clone)]
pub enum DatabaseBackend {
    Rocksdb { config: RocksConfig },
    Paritydb { options: ParityOptions, columns: u8 },
}

#[derive(Clone)]
pub struct DatabaseSettings {
    pub path: PathBuf,
    pub backend: DatabaseBackend,
}

#[derive(Debug, Clone)]
pub enum ParityCompression {
    Snappy,
    Lz4,
}

impl FromStr for ParityCompression {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "snappy" => Ok(ParityCompression::Snappy),
            "lz4" => Ok(ParityCompression::Lz4),
            other => Err(format!(
                "Invalid paritydb compression '{other}'. Expected snappy/lz4"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParityDbOpenConfig {
    pub columns: u32,
    pub compression: Option<ParityCompression>,
    pub disable_wal: bool,
    pub stats: bool,
}

impl Default for ParityDbOpenConfig {
    fn default() -> Self {
        Self { columns: 0, compression: None, disable_wal: false, stats: false }
    }
}

pub fn rocksdb_settings(
    path: PathBuf, db_cache_size: Option<usize>,
    db_compaction: DatabaseCompactionProfile, columns: u32, disable_wal: bool,
) -> DatabaseSettings {
    let mut config = RocksConfig::with_columns(columns);
    config.memory_budget = db_cache_size;
    config.compaction = compaction_profile(&db_compaction, &path);
    config.disable_wal = disable_wal;

    DatabaseSettings { path, backend: DatabaseBackend::Rocksdb { config } }
}

pub fn paritydb_settings(
    path: PathBuf, config: &ParityDbOpenConfig,
) -> io::Result<DatabaseSettings> {
    fs::create_dir_all(&path)?;

    let columns: u8 = config
        .columns
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "paritydb columns must fit into u8"))?;

    let mut options = ParityOptions::with_columns(&path, columns);
    options.sync_wal = !config.disable_wal;
    options.sync_data = !config.disable_wal;
    options.stats = config.stats;

    let compression = match config.compression {
        None => CompressionType::NoCompression,
        Some(ParityCompression::Snappy) => CompressionType::Snappy,
        Some(ParityCompression::Lz4) => CompressionType::Lz4,
    };

    for column in options.columns.iter_mut() {
        column.compression = compression;
        // Enable ordered iteration. This is required for the kvdb iteration contract.
        column.btree_index = true;
    }

    Ok(DatabaseSettings {
        path,
        backend: DatabaseBackend::Paritydb { options, columns },
    })
}

pub fn open_database(settings: &DatabaseSettings) -> io::Result<Arc<SystemDB>> {
    match &settings.backend {
        DatabaseBackend::Rocksdb { config } => {
            fs::create_dir_all(&settings.path)?;
            let db = match RocksDatabase::open(config, settings.path.to_str().unwrap()) {
                Ok(db) => {
                    info!("Open RocksDB successfully ({:?})", settings.path);
                    db
                }
                Err(e) => {
                    warn!("Failed to open RocksDB ({:?})", settings.path);
                    return Err(e);
                }
            };
            let db = Arc::new(db);
            let dyn_db: Arc<dyn KeyValueStore> = db.clone();
            let sys_db = SystemDB::new(dyn_db);
            Ok(Arc::new(sys_db))
        }
        DatabaseBackend::Paritydb { options, columns } => {
            let mut options = options.clone();
            options.path = settings.path.clone();
            let parity_db = ParityDb::open_or_create(&options)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            info!("Open ParityDB successfully ({:?})", settings.path);
            let kvdb: Arc<dyn KeyValueStore> = Arc::new(ParityKeyValueDb::new(parity_db, *columns));
            let sys_db = SystemDB::new(kvdb);
            Ok(Arc::new(sys_db))
        }
    }
}

fn compaction_profile(profile: &DatabaseCompactionProfile, db_path: &Path) -> CompactionProfile {
    match profile {
        DatabaseCompactionProfile::Auto => CompactionProfile::auto(db_path),
        DatabaseCompactionProfile::SSD => CompactionProfile::ssd(),
        DatabaseCompactionProfile::HDD => CompactionProfile::hdd(),
    }
}

#[derive(Clone)]
struct ParityKeyValueDb {
    inner: Arc<ParityDb>,
    columns: u8,
}

impl ParityKeyValueDb {
    fn new(inner: ParityDb, columns: u8) -> Self {
        Self { inner: Arc::new(inner), columns }
    }

    fn col_id(&self, col: u32) -> io::Result<parity_db::ColId> {
        let col_u8: u8 = col
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "column out of range"))?;
        if col_u8 >= self.columns {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "column out of range"));
        }
        Ok(col_u8)
    }

    fn iterator<'a>(
        &'a self, col: parity_db::ColId, prefix: Option<&'a [u8]>,
    ) -> io::Result<ParityIterator<'a>> {
        let mut iter = self
            .inner
            .iter(col)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        if let Some(prefix) = prefix {
            iter.seek(prefix)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        } else {
            iter.seek_to_first()
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        }
        Ok(ParityIterator { inner: iter })
    }
}

impl MallocSizeOf for ParityKeyValueDb {
    fn size_of(&self, _ops: &mut MallocSizeOfOps) -> usize {
        0
    }
}

impl KeyValueDB for ParityKeyValueDb {
    fn get(&self, col: u32, key: &[u8]) -> io::Result<Option<DBValue>> {
        let col = self.col_id(col)?;
        self.inner
            .get(col, key)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    fn get_by_prefix(&self, col: u32, prefix: &[u8]) -> Option<Box<[u8]>> {
        let col = self.col_id(col).ok()?;
        let mut iter = self.iterator(col, Some(prefix)).ok()?;
        iter.find(|(key, _)| key.starts_with(prefix)).map(|(k, _)| k)
    }

    fn write_buffered(&self, transaction: DBTransaction) {
        if transaction.ops.is_empty() {
            return;
        }
        let mut buffer = Vec::with_capacity(transaction.ops.len());
        for op in transaction.ops {
            match op {
                DBOp::Insert { col, key, value } => match self.col_id(col) {
                    Ok(col_id) => buffer.push((col_id, key.to_vec(), Some(value))),
                    Err(e) => {
                        error!("ParityDB write failed: {}", e);
                        return;
                    }
                },
                DBOp::Delete { col, key } => match self.col_id(col) {
                    Ok(col_id) => buffer.push((col_id, key.to_vec(), None)),
                    Err(e) => {
                        error!("ParityDB write failed: {}", e);
                        return;
                    }
                },
            }
        }
        if let Err(e) = self.inner.commit(buffer) {
            error!("ParityDB commit error: {e}");
        }
    }

    fn flush(&self) -> io::Result<()> {
        (&*self.inner)
            .flush_logs()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    fn iter<'a>(&'a self, col: u32) -> Box<dyn Iterator<Item = (Box<[u8]>, Box<[u8]>)> + 'a> {
        match self.col_id(col).and_then(|id| self.iterator(id, None)) {
            Ok(iter) => Box::new(iter),
            Err(e) => {
                warn!("ParityDB iteration failed: {}", e);
                Box::new(iter::empty::<(Box<[u8]>, Box<[u8]>)>())
            }
        }
    }

    fn iter_from_prefix<'a>(
        &'a self, col: u32, prefix: &'a [u8],
    ) -> Box<dyn Iterator<Item = (Box<[u8]>, Box<[u8]>)> + 'a> {
        match self.col_id(col).and_then(|id| self.iterator(id, Some(prefix))) {
            Ok(iter) => {
                let prefix_vec = prefix.to_vec();
                Box::new(iter.take_while(move |(key, _)| key.starts_with(prefix_vec.as_slice())))
            }
            Err(e) => {
                warn!("ParityDB iteration failed: {}", e);
                Box::new(iter::empty::<(Box<[u8]>, Box<[u8]>)>())
            }
        }
    }

    fn restore(&self, _new_db: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "ParityDB restore is not supported",
        ))
    }

    fn io_stats(&self, _kind: IoStatsKind) -> IoStats {
        IoStats::empty()
    }
}

struct ParityIterator<'a> {
    inner: parity_db::BTreeIterator<'a>,
}

impl<'a> Iterator for ParityIterator<'a> {
    type Item = (Box<[u8]>, Box<[u8]>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.inner.next() {
                Ok(Some((key, value))) => {
                    return Some((key.into_boxed_slice(), value.into_boxed_slice()))
                }
                Ok(None) => return None,
                Err(e) => {
                    warn!("ParityDB iterator error: {}", e);
                    return None;
                }
            }
        }
    }
}
