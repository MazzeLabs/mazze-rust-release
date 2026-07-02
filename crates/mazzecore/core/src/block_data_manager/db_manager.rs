use crate::{
    block_data_manager::{
        db_decode_list, db_encode_list, BlamedHeaderVerifiedRoots,
        BlockExecutionResultWithEpoch, BlockRewardResult, BlockTracesWithEpoch,
        CheckpointHashes, DataVersionTuple, EpochExecutionContext,
        LocalBlockInfo,
    },
    db::{
        COL_BLAMED_HEADER_VERIFIED_ROOTS, COL_BLOCKS, COL_BLOCK_TRACES,
        COL_EPOCH_NUMBER, COL_HASH_BY_BLOCK_NUMBER, COL_MISC, COL_TX_INDEX,
    },
    pow::PowComputer,
    verification::VerificationConfig,
};
use byteorder::{ByteOrder, LittleEndian};
use db::SystemDB;
use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};
use mazze_internal_common::{
    DatabaseDecodable, DatabaseEncodable, EpochExecutionCommitment,
};
use mazze_parameters::pow::RANDOMX_EPOCH_LENGTH;
use mazze_storage::{
    storage_db::KeyValueDbTrait, KvdbMdbx, KvdbParitydb, MdbxColumn, MdbxEnv,
    MdbxShadowMirror,
};
use mazze_types::H256;
use primitives::{Block, BlockHeader, SignedTransaction, TransactionIndex};
use metrics::{Counter, CounterUsize};
use rlp::Rlp;
use std::{collections::HashMap, sync::Arc};
use strum::IntoEnumIterator;
use strum_macros::EnumIter;

lazy_static! {
    /// Counter of failed DB writes. Each increment indicates the in-memory
    /// cache and the persistent store have diverged for one key.
    /// See docs/flow-audit.md G-TX-3.
    static ref DB_WRITE_FAILURES: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group("db", "write_failures");
}

const LOCAL_BLOCK_INFO_SUFFIX_BYTE: u8 = 1;
const BLOCK_BODY_SUFFIX_BYTE: u8 = 2;
const BLOCK_EXECUTION_RESULT_SUFFIX_BYTE: u8 = 3;
const EPOCH_EXECUTION_CONTEXT_SUFFIX_BYTE: u8 = 4;
const EPOCH_CONSENSUS_EXECUTION_INFO_SUFFIX_BYTE: u8 = 5;
const EPOCH_EXECUTED_BLOCK_SET_SUFFIX_BYTE: u8 = 6;
const EPOCH_SKIPPED_BLOCK_SET_SUFFIX_BYTE: u8 = 7;
const BLOCK_REWARD_RESULT_SUFFIX_BYTE: u8 = 8;
const BLOCK_TERMINAL_KEY: &[u8] = b"block_terminals";
const GC_PROGRESS_KEY: &[u8] = b"gc_progress";

#[derive(Clone, Copy, Debug, Hash, Ord, PartialOrd, Eq, PartialEq, EnumIter)]
pub enum DBTable {
    Misc,
    Blocks,
    Transactions,
    EpochNumbers,
    BlamedHeaderVerifiedRoots,
    BlockTraces,
    HashByBlockNumber,
}
fn rocks_db_col(table: DBTable) -> u32 {
    match table {
        DBTable::Misc => COL_MISC,
        DBTable::Blocks => COL_BLOCKS,
        DBTable::Transactions => COL_TX_INDEX,
        DBTable::EpochNumbers => COL_EPOCH_NUMBER,
        DBTable::BlamedHeaderVerifiedRoots => COL_BLAMED_HEADER_VERIFIED_ROOTS,
        DBTable::BlockTraces => COL_BLOCK_TRACES,
        DBTable::HashByBlockNumber => COL_HASH_BY_BLOCK_NUMBER,
    }
}

/// Which `DBTable`s should have their writes routed through an MDBX
/// shadow mirror. Each field defaults to `false`; operators enable
/// individual tables via `enable_mdbx_shadow_*` keys in `hydra.toml`.
/// Adding a new shadowed table is: (a) a new field here, (b) a new
/// arm in [`DBManager::build_shadow_mirrors`], (c) a new key in
/// [`DataManagerConfiguration`](super::DataManagerConfiguration) and
/// [`configuration.rs`](../../../../../../client/src/configuration.rs).
#[derive(Clone, Copy, Debug, Default)]
pub struct MdbxShadowFlags {
    pub hash_by_block_number: bool,
    pub tx_index: bool,
}

pub struct DBManager {
    table_db: HashMap<DBTable, Box<dyn KeyValueDbTrait<ValueType = Box<[u8]>>>>,
    pow: Arc<PowComputer>,
    genesis_hash: H256,
    /// MDBX hot-tier environment, shared with `StorageManager`. `None`
    /// when the storage layer runs ParityDB-only (dev fallback); `Some`
    /// once the storage rewrite starts opening its env at startup
    /// (see `storage_manager.rs:132`).
    mdbx_env: Option<Arc<MdbxEnv>>,
    /// Storage Phase 2 opt-in: per-`DBTable` shadow mirrors. Only
    /// tables the operator enabled AND that have an MDBX env open
    /// appear as keys here. Every write to a shadowed table
    /// [`Self::insert_to_db`] / [`Self::remove_from_db`] routes
    /// through the mirror, which writes to BOTH the current ParityDB
    /// column (unchanged behavior for readers) AND an MDBX shadow
    /// column. `MdbxShadowMirror::verify_parity` (called at era
    /// boundaries or via `debug_mdbxShadowVerifyParity`) walks both
    /// sides and reports divergence per table.
    ///
    /// Empty map = all shadowing disabled: writes fall through to the
    /// plain `table_db`, byte-for-byte the same as before Phase 2.
    /// This lets each flag be flipped independently and rolled back
    /// cleanly if a mirror shows pathological divergence.
    mdbx_shadow_mirrors:
        HashMap<DBTable, Arc<MdbxShadowMirror<KvdbParitydb>>>,
}

impl DBManager {
    fn new_from_kvdb(
        db: Arc<SystemDB>, pow: Arc<PowComputer>, genesis_hash: H256,
        mdbx_env: Option<Arc<MdbxEnv>>,
        shadow_flags: MdbxShadowFlags,
    ) -> Self {
        let mut table_db = HashMap::new();

        for table in DBTable::iter() {
            table_db.insert(
                table,
                Box::new(KvdbParitydb {
                    kvdb: db.key_value(),
                    col: rocks_db_col(table),
                })
                    as Box<dyn KeyValueDbTrait<ValueType = Box<[u8]>>>,
            );
        }

        let mdbx_shadow_mirrors = Self::build_shadow_mirrors(
            &db,
            mdbx_env.as_ref(),
            shadow_flags,
        );

        Self {
            table_db,
            pow,
            genesis_hash,
            mdbx_env,
            mdbx_shadow_mirrors,
        }
    }

    /// Construct the per-table shadow-mirror map. For every
    /// `DBTable` that the operator opted into AND for which we have
    /// an MDBX env open, we wrap the same ParityDB column
    /// `table_db[table]` writes into by a `KvdbParitydb` primary
    /// (so no double-write on the primary side) and pair it with a
    /// `KvdbMdbx` over the matching `MdbxColumn`. The mirror routes
    /// writes to both sides (see `MdbxShadowMirror::put` for the
    /// atomicity note).
    fn build_shadow_mirrors(
        db: &Arc<SystemDB>, mdbx_env: Option<&Arc<MdbxEnv>>,
        flags: MdbxShadowFlags,
    ) -> HashMap<DBTable, Arc<MdbxShadowMirror<KvdbParitydb>>> {
        let mut mirrors = HashMap::new();
        let env = match mdbx_env {
            Some(e) => e,
            // No MDBX env → nothing to shadow into.
            None => return mirrors,
        };

        let make_mirror =
            |table: DBTable, col: MdbxColumn| -> Arc<MdbxShadowMirror<KvdbParitydb>> {
                let primary_pdb = KvdbParitydb {
                    kvdb: db.key_value(),
                    col: rocks_db_col(table),
                };
                let shadow_mdbx =
                    KvdbMdbx::with_column(Arc::clone(env), col.id());
                Arc::new(MdbxShadowMirror::new(
                    primary_pdb,
                    shadow_mdbx,
                    col,
                ))
            };

        if flags.hash_by_block_number {
            mirrors.insert(
                DBTable::HashByBlockNumber,
                make_mirror(
                    DBTable::HashByBlockNumber,
                    MdbxColumn::HashByNumber,
                ),
            );
        }
        if flags.tx_index {
            mirrors.insert(
                DBTable::Transactions,
                make_mirror(DBTable::Transactions, MdbxColumn::TxIndex),
            );
        }
        mirrors
    }

    pub fn new_from_paritydb(
        db: Arc<SystemDB>, pow: Arc<PowComputer>, genesis_hash: H256,
        mdbx_env: Option<Arc<MdbxEnv>>, shadow_flags: MdbxShadowFlags,
    ) -> Self {
        Self::new_from_kvdb(db, pow, genesis_hash, mdbx_env, shadow_flags)
    }

    /// The MDBX hot-tier env attached to this manager, if the storage
    /// layer opened one at startup. Consumers add per-table
    /// `MdbxShadowMirror` fields on this manager and initialize them
    /// against columns opened on this env.
    #[allow(dead_code)]
    pub(crate) fn mdbx_env(&self) -> Option<Arc<MdbxEnv>> {
        self.mdbx_env.clone()
    }

    /// A specific shadow mirror keyed by `DBTable`, if that table
    /// is currently shadowed. Exposed for targeted verify calls;
    /// dashboards typically want [`Self::mdbx_shadow_mirrors`] to
    /// iterate every active mirror.
    #[allow(dead_code)]
    pub fn mdbx_shadow_mirror(
        &self, table: DBTable,
    ) -> Option<Arc<MdbxShadowMirror<KvdbParitydb>>> {
        self.mdbx_shadow_mirrors.get(&table).cloned()
    }

    /// Snapshot of every active shadow mirror keyed by `DBTable`.
    /// Used by `debug_mdbxShadowVerifyParity` to walk every mirror
    /// in one RPC roundtrip. Cheap `Arc` clone per entry — the map
    /// itself is rebuilt so callers can iterate without holding a
    /// borrow on `DBManager`.
    pub fn mdbx_shadow_mirrors(
        &self,
    ) -> HashMap<DBTable, Arc<MdbxShadowMirror<KvdbParitydb>>> {
        self.mdbx_shadow_mirrors.clone()
    }
}

impl DBManager {
    pub fn insert_block_traces_to_db(
        &self, block_hash: &H256, block_traces: &BlockTracesWithEpoch,
    ) {
        self.insert_encodable_val(
            DBTable::BlockTraces,
            block_hash.as_bytes(),
            block_traces,
        );
    }

    pub fn block_traces_from_db(
        &self, block_hash: &H256,
    ) -> Option<BlockTracesWithEpoch> {
        let block_traces = self
            .load_decodable_val(DBTable::BlockTraces, block_hash.as_bytes())?;
        Some(block_traces)
    }

    /// TODO Use new_with_rlp_size
    pub fn block_from_db(&self, block_hash: &H256) -> Option<Block> {
        Some(Block::new(
            self.block_header_from_db(block_hash)?,
            self.block_body_from_db(block_hash)?,
        ))
    }

    pub fn insert_block_header_to_db(&self, header: &BlockHeader) {
        self.insert_encodable_val(
            DBTable::Blocks,
            header.hash().as_bytes(),
            header,
        );
    }

    pub fn block_header_from_db(&self, hash: &H256) -> Option<BlockHeader> {
        let mut block_header: BlockHeader =
            self.load_decodable_val(DBTable::Blocks, hash.as_bytes())?;

        let seed_hash = self.get_current_seed_hash(block_header.height());

        VerificationConfig::get_or_fill_header_pow_quality(
            &self.pow,
            &mut block_header,
            &seed_hash,
        );

        Some(block_header)
    }

    pub fn remove_block_header_from_db(&self, hash: &H256) {
        self.remove_from_db(DBTable::Blocks, hash.as_bytes());
    }

    pub fn insert_transaction_index_to_db(
        &self, hash: &H256, value: &TransactionIndex,
    ) {
        self.insert_encodable_val(DBTable::Transactions, hash.as_bytes(), value)
    }

    pub fn transaction_index_from_db(
        &self, hash: &H256,
    ) -> Option<TransactionIndex> {
        self.load_decodable_val(DBTable::Transactions, hash.as_bytes())
    }

    pub fn insert_hash_by_block_number_to_db(
        &self, block_number: u64, hash: &H256,
    ) {
        self.insert_encodable_val(
            DBTable::HashByBlockNumber,
            &block_number.to_be_bytes(),
            hash,
        )
    }

    pub fn hash_by_block_number_from_db(
        &self, block_number: &u64,
    ) -> Option<H256> {
        self.load_decodable_val(
            DBTable::HashByBlockNumber,
            &block_number.to_be_bytes(),
        )
    }

    /// Store block info to db. Block info includes block status and
    /// the sequence number when the block enters consensus graph.
    /// The db key is the block hash plus one extra byte, so we can get better
    /// data locality if we get both a block and its info from db.
    /// The info is not a part of the block because the block is inserted
    /// before we know its info, and we do not want to insert a large chunk
    /// again. TODO Maybe we can use in-place modification to keep the info
    /// together with the block.
    pub fn insert_local_block_info_to_db(
        &self, block_hash: &H256, value: &LocalBlockInfo,
    ) {
        self.insert_encodable_val(
            DBTable::Blocks,
            &local_block_info_key(block_hash),
            value,
        );
    }

    /// Get block info from db.
    pub fn local_block_info_from_db(
        &self, block_hash: &H256,
    ) -> Option<LocalBlockInfo> {
        self.load_decodable_val(
            DBTable::Blocks,
            &local_block_info_key(block_hash),
        )
    }

    pub fn insert_blamed_header_verified_roots_to_db(
        &self, block_height: u64, value: &BlamedHeaderVerifiedRoots,
    ) {
        self.insert_encodable_val(
            DBTable::BlamedHeaderVerifiedRoots,
            &blamed_header_verified_roots_key(block_height),
            value,
        );
    }

    /// Get correct roots of blamed headers from db.
    /// These are maintained on light nodes only.
    pub fn blamed_header_verified_roots_from_db(
        &self, block_height: u64,
    ) -> Option<BlamedHeaderVerifiedRoots> {
        self.load_decodable_val(
            DBTable::BlamedHeaderVerifiedRoots,
            &blamed_header_verified_roots_key(block_height),
        )
    }

    pub fn remove_blamed_header_verified_roots_from_db(
        &self, block_height: u64,
    ) {
        self.remove_from_db(
            DBTable::BlamedHeaderVerifiedRoots,
            &blamed_header_verified_roots_key(block_height),
        )
    }

    pub fn insert_block_body_to_db(&self, block: &Block) {
        self.insert_to_db(
            DBTable::Blocks,
            &block_body_key(&block.hash()),
            block.encode_body_with_tx_public(),
        )
    }

    pub fn block_body_from_db(
        &self, hash: &H256,
    ) -> Option<Vec<Arc<SignedTransaction>>> {
        let encoded =
            self.load_from_db(DBTable::Blocks, &block_body_key(hash))?;
        let rlp = Rlp::new(&encoded);
        Some(
            Block::decode_body_with_tx_public(&rlp)
                .expect("Wrong block rlp format!"),
        )
    }

    pub fn remove_block_body_from_db(&self, hash: &H256) {
        self.remove_from_db(DBTable::Blocks, &block_body_key(hash))
    }

    pub fn insert_block_execution_result_to_db(
        &self, hash: &H256, value: &BlockExecutionResultWithEpoch,
    ) {
        self.insert_encodable_val(
            DBTable::Blocks,
            &block_execution_result_key(hash),
            value,
        )
    }

    pub fn insert_block_reward_result_to_db(
        &self, hash: &H256, value: &DataVersionTuple<H256, BlockRewardResult>,
    ) {
        self.insert_encodable_val(
            DBTable::Blocks,
            &block_reward_result_key(hash),
            value,
        )
    }

    pub fn block_execution_result_from_db(
        &self, hash: &H256,
    ) -> Option<BlockExecutionResultWithEpoch> {
        self.load_decodable_val(
            DBTable::Blocks,
            &block_execution_result_key(hash),
        )
    }

    pub fn block_reward_result_from_db(
        &self, hash: &H256,
    ) -> Option<DataVersionTuple<H256, BlockRewardResult>> {
        self.load_might_decodable_val(
            DBTable::Blocks,
            &block_reward_result_key(hash),
        )
    }

    pub fn remove_block_execution_result_from_db(&self, hash: &H256) {
        self.remove_from_db(DBTable::Blocks, &block_execution_result_key(hash))
    }

    pub fn remove_block_reward_result_from_db(&self, hash: &H256) {
        self.remove_from_db(DBTable::Blocks, &block_reward_result_key(hash))
    }

    pub fn remove_block_trace_from_db(&self, hash: &H256) {
        self.remove_from_db(DBTable::BlockTraces, hash.as_bytes())
    }

    pub fn remove_transaction_index_from_db(&self, hash: &H256) {
        self.remove_from_db(DBTable::Transactions, hash.as_bytes())
    }

    /// Persist the current and previous checkpoint hashes.
    ///
    /// C.3 — Monotonicity gate. The caller passes `new_epoch_number`,
    /// which is the epoch number of the new `checkpoint_cur` block.
    /// We compare against the previously persisted epoch number (key
    /// `b"checkpoint_epoch"`); if the new value is *strictly less*
    /// than the old, the write is **refused** and an `error!` is
    /// logged — that situation indicates either a code-path that
    /// shouldn't exist or DB corruption.
    ///
    /// Returns `Ok(())` on a successful write, `Err` on monotonicity
    /// violation. Equality is allowed (idempotent re-writes during
    /// recovery / replay are fine).
    ///
    /// See `docs/checkpoint-snapshot-lifecycle.md` Phase C.3.
    pub fn insert_checkpoint_hashes_to_db(
        &self, checkpoint_prev: &H256, checkpoint_cur: &H256,
        new_epoch_number: u64,
    ) -> Result<(), String> {
        // Monotonicity gate. Read the previously persisted epoch
        // number from the dedicated `b"checkpoint_epoch"` key. Missing
        // → first checkpoint of this run; accept.
        if let Some(prev_epoch_number) =
            self.checkpoint_epoch_number_from_db()
        {
            if new_epoch_number < prev_epoch_number {
                let msg = format!(
                    "insert_checkpoint_hashes_to_db REFUSED: non-monotonic write \
                     (prev_epoch={}, new_epoch={}, new_hash={:?}). \
                     This indicates a code-path violating the consensus invariant \
                     that cur_era_genesis_height is monotonically non-decreasing.",
                    prev_epoch_number, new_epoch_number, checkpoint_cur
                );
                error!("{}", msg);
                return Err(msg);
            }
        }

        self.insert_encodable_val(
            DBTable::Misc,
            b"checkpoint",
            &CheckpointHashes::new(*checkpoint_prev, *checkpoint_cur),
        );
        // Persist the epoch number separately for the monotonicity
        // gate above. RLP-encoded u64.
        self.insert_encodable_val(
            DBTable::Misc,
            b"checkpoint_epoch",
            &new_epoch_number,
        );
        Ok(())
    }

    pub fn checkpoint_hashes_from_db(&self) -> Option<(H256, H256)> {
        let checkpoints: CheckpointHashes =
            self.load_decodable_val(DBTable::Misc, b"checkpoint")?;
        Some((checkpoints.prev_hash, checkpoints.cur_hash))
    }

    /// Companion to `insert_checkpoint_hashes_to_db`. Returns `None`
    /// when no checkpoint has been persisted yet (fresh DB or a DB
    /// written before this field was introduced).
    pub fn checkpoint_epoch_number_from_db(&self) -> Option<u64> {
        self.load_decodable_val(DBTable::Misc, b"checkpoint_epoch")
    }

    pub fn insert_executed_epoch_set_hashes_to_db(
        &self, epoch: u64, executed_hashes: &Vec<H256>,
    ) {
        self.insert_encodable_list(
            DBTable::EpochNumbers,
            &executed_epoch_set_key(epoch)[0..9],
            executed_hashes,
        );
    }

    pub fn insert_skipped_epoch_set_hashes_to_db(
        &self, epoch: u64, skipped_hashes: &Vec<H256>,
    ) {
        self.insert_encodable_list(
            DBTable::EpochNumbers,
            &skipped_epoch_set_key(epoch)[0..9],
            skipped_hashes,
        );
    }

    pub fn executed_epoch_set_hashes_from_db(
        &self, epoch: u64,
    ) -> Option<Vec<H256>> {
        self.load_decodable_list(
            DBTable::EpochNumbers,
            &executed_epoch_set_key(epoch)[0..9],
        )
    }

    pub fn skipped_epoch_set_hashes_from_db(
        &self, epoch: u64,
    ) -> Option<Vec<H256>> {
        self.load_decodable_list(
            DBTable::EpochNumbers,
            &skipped_epoch_set_key(epoch)[0..9],
        )
    }

    pub fn insert_terminals_to_db(&self, terminals: &Vec<H256>) {
        self.insert_encodable_list(
            DBTable::Misc,
            BLOCK_TERMINAL_KEY,
            terminals,
        );
    }

    pub fn terminals_from_db(&self) -> Option<Vec<H256>> {
        self.load_decodable_list(DBTable::Misc, BLOCK_TERMINAL_KEY)
    }

    pub fn insert_epoch_execution_commitment_to_db(
        &self, hash: &H256, ctx: &EpochExecutionCommitment,
    ) {
        self.insert_encodable_val(
            DBTable::Blocks,
            &epoch_consensus_epoch_execution_commitment_key(hash),
            ctx,
        );
    }

    pub fn epoch_execution_commitment_from_db(
        &self, hash: &H256,
    ) -> Option<EpochExecutionCommitment> {
        self.load_decodable_val(
            DBTable::Blocks,
            &epoch_consensus_epoch_execution_commitment_key(hash),
        )
    }

    pub fn remove_epoch_execution_commitment_from_db(&self, hash: &H256) {
        self.remove_from_db(
            DBTable::Blocks,
            &epoch_consensus_epoch_execution_commitment_key(hash),
        );
    }

    pub fn insert_instance_id_to_db(&self, instance_id: u64) {
        self.insert_encodable_val(DBTable::Misc, b"instance", &instance_id);
    }

    pub fn instance_id_from_db(&self) -> Option<u64> {
        self.load_decodable_val(DBTable::Misc, b"instance")
    }

    pub fn insert_execution_context_to_db(
        &self, hash: &H256, ctx: &EpochExecutionContext,
    ) {
        self.insert_encodable_val(
            DBTable::Blocks,
            &epoch_execution_context_key(hash),
            ctx,
        )
    }

    pub fn execution_context_from_db(
        &self, hash: &H256,
    ) -> Option<EpochExecutionContext> {
        self.load_decodable_val(
            DBTable::Blocks,
            &epoch_execution_context_key(hash),
        )
    }

    pub fn remove_epoch_execution_context_from_db(&self, hash: &H256) {
        self.remove_from_db(DBTable::Blocks, &epoch_execution_context_key(hash))
    }

    pub fn insert_gc_progress_to_db(&self, next_to_process: u64) {
        self.insert_encodable_val(
            DBTable::Misc,
            GC_PROGRESS_KEY,
            &next_to_process,
        );
    }

    pub fn gc_progress_from_db(&self) -> Option<u64> {
        self.load_decodable_val(DBTable::Misc, GC_PROGRESS_KEY)
    }

    /// The functions below are private utils used by the DBManager to access
    /// database
    fn insert_to_db(&self, table: DBTable, db_key: &[u8], value: Vec<u8>) {
        // Storage Phase 2 shadow route: if the operator has enabled
        // MDBX shadow-mirroring for this table, route the write
        // through the mirror. The mirror wraps the SAME ParityDB
        // column that `table_db[table]` would have written to, so
        // there's no double-write on the primary side; the mirror
        // just also flushes to the MDBX shadow column atomically.
        // Falls through to the plain path on mirror error so a
        // shadow-side hiccup doesn't drop primary writes.
        if let Some(mirror) = self.mdbx_shadow_mirrors.get(&table) {
            match mirror.put(db_key, &value) {
                Ok(()) => return,
                Err(e) => {
                    DB_WRITE_FAILURES.inc(1);
                    error!(
                        "mdbx-shadow put failed for {:?} key_len={}: \
                         {:?}. Falling back to primary-only write",
                        table,
                        db_key.len(),
                        e
                    );
                    // Fall through to the normal path so the
                    // primary still lands the write.
                }
            }
        }
        if let Err(e) =
            self.table_db.get(&table).unwrap().put(db_key, &value)
        {
            // G-TX-3 in docs/flow-audit.md. A failed write here means the
            // in-memory cache and the on-disk DB diverge: the cache has
            // the value but disk does not. We log loudly so corruption
            // surfaces in operator dashboards rather than only as
            // mysterious-missing-data later. Counter exposed so
            // dashboards can alert.
            DB_WRITE_FAILURES.inc(1);
            error!(
                "db insertion failed for table {:?} key_len={} value_len={}: \
                 {:?}. CACHE/DB DIVERGENCE",
                table,
                db_key.len(),
                value.len(),
                e
            );
        }
    }

    fn remove_from_db(&self, table: DBTable, db_key: &[u8]) {
        // Shadow-mirror route (see `insert_to_db` for rationale).
        if let Some(mirror) = self.mdbx_shadow_mirrors.get(&table) {
            match mirror.delete(db_key) {
                Ok(()) => return,
                Err(e) => {
                    DB_WRITE_FAILURES.inc(1);
                    error!(
                        "mdbx-shadow delete failed for {:?} \
                         key_len={}: {:?}. Falling back to \
                         primary-only delete",
                        table,
                        db_key.len(),
                        e
                    );
                }
            }
        }
        self.table_db
            .get(&table)
            .unwrap()
            .delete(db_key)
            .expect("db removal failure");
    }

    fn load_from_db(&self, table: DBTable, db_key: &[u8]) -> Option<Box<[u8]>> {
        match self.table_db.get(&table).unwrap().get(db_key) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    "db read failure ignored for table {:?}, key_len={}: {:?}",
                    table,
                    db_key.len(),
                    e
                );
                None
            }
        }
    }

    fn insert_encodable_val<V>(&self, table: DBTable, db_key: &[u8], value: &V)
    where
        V: DatabaseEncodable,
    {
        self.insert_to_db(table, db_key, value.db_encode())
    }

    fn insert_encodable_list<V>(
        &self, table: DBTable, db_key: &[u8], value: &Vec<V>,
    ) where
        V: DatabaseEncodable,
    {
        self.insert_to_db(table, db_key, db_encode_list(value))
    }

    fn load_decodable_val<V>(&self, table: DBTable, db_key: &[u8]) -> Option<V>
    where
        V: DatabaseDecodable,
    {
        let encoded = self.load_from_db(table, db_key)?;
        match V::db_decode(&encoded) {
            Ok(v) => Some(v),
            Err(e) => {
                warn!(
                    "db decode failure ignored for table {:?}, key_len={}: {:?}",
                    table,
                    db_key.len(),
                    e
                );
                None
            }
        }
    }

    fn load_might_decodable_val<V>(
        &self, table: DBTable, db_key: &[u8],
    ) -> Option<V>
    where
        V: DatabaseDecodable,
    {
        let encoded = self.load_from_db(table, db_key)?;
        V::db_decode(&encoded).ok()
    }

    fn load_decodable_list<V>(
        &self, table: DBTable, db_key: &[u8],
    ) -> Option<Vec<V>>
    where
        V: DatabaseDecodable,
    {
        let encoded = self.load_from_db(table, db_key)?;
        match db_decode_list(&encoded) {
            Ok(v) => Some(v),
            Err(e) => {
                warn!(
                    "db decode list failure ignored for table {:?}, key_len={}: {:?}",
                    table,
                    db_key.len(),
                    e
                );
                None
            }
        }
    }

    pub fn get_current_seed_hash(&self, epoch_height: u64) -> H256 {
        match self.try_get_current_seed_hash(epoch_height) {
            Some(h) => h,
            None => {
                // Lookup miss in a non-genesis epoch is an anomaly —
                // the seed for this block's RandomX epoch should
                // exist in the DB by the time we reach verification.
                // Logging `error!` rather than panicking so the node
                // stays up, but callers that *gate consensus* on this
                // (sync_graph::insert_block_header) MUST use
                // `try_get_current_seed_hash` and reject the block
                // instead of accepting a default-zero seed. See
                // docs/security-audit.md finding H-1.
                error!(
                    "get_current_seed_hash: seed lookup MISSED for epoch_height={}; returning H256::zero(). This indicates DB inconsistency.",
                    epoch_height
                );
                H256::zero()
            }
        }
    }

    /// Result-shaped variant of `get_current_seed_hash`. Returns
    /// `Some(genesis_hash)` for the genesis-window (epoch 0), the
    /// looked-up seed for non-genesis epochs when it exists, and
    /// `None` when the seed lookup misses — the block whose seed this
    /// would have been is, at the very least, *unverifiable* on this
    /// node, and consensus-critical callers (the sync graph) must
    /// reject rather than fall back to a zero seed.
    ///
    /// See docs/security-audit.md finding H-1.
    pub fn try_get_current_seed_hash(
        &self, epoch_height: u64,
    ) -> Option<H256> {
        let current_epoch = epoch_height / RANDOMX_EPOCH_LENGTH;

        // Genesis window — seed is the genesis hash, always available.
        if current_epoch == 0 {
            return Some(self.genesis_hash);
        }

        // Non-genesis epochs: look up the block at the start of the
        // previous RandomX epoch.
        let seed_height = (current_epoch - 1) * RANDOMX_EPOCH_LENGTH;
        let seed_hash = self
            .executed_epoch_set_hashes_from_db(seed_height)
            .and_then(|hashes| hashes.last().cloned());
        trace!(
            "try_get_current_seed_hash: epoch={} seed_height={} result={:?}",
            current_epoch,
            seed_height,
            seed_hash
        );
        seed_hash
    }
}

fn append_suffix(h: &H256, suffix: u8) -> Vec<u8> {
    let mut key = Vec::with_capacity(H256::len_bytes() + 1);
    key.extend_from_slice(h.as_bytes());
    key.push(suffix);
    key
}

fn local_block_info_key(block_hash: &H256) -> Vec<u8> {
    append_suffix(block_hash, LOCAL_BLOCK_INFO_SUFFIX_BYTE)
}

fn blamed_header_verified_roots_key(block_height: u64) -> [u8; 8] {
    let mut height_key = [0; 8];
    LittleEndian::write_u64(&mut height_key[0..8], block_height);
    height_key
}

fn block_body_key(block_hash: &H256) -> Vec<u8> {
    append_suffix(block_hash, BLOCK_BODY_SUFFIX_BYTE)
}

fn executed_epoch_set_key(epoch_number: u64) -> [u8; 9] {
    let mut epoch_key = [0; 9];
    LittleEndian::write_u64(&mut epoch_key[0..8], epoch_number);
    epoch_key[8] = EPOCH_EXECUTED_BLOCK_SET_SUFFIX_BYTE;
    epoch_key
}

fn skipped_epoch_set_key(epoch_number: u64) -> [u8; 9] {
    let mut epoch_key = [0; 9];
    LittleEndian::write_u64(&mut epoch_key[0..8], epoch_number);
    epoch_key[8] = EPOCH_SKIPPED_BLOCK_SET_SUFFIX_BYTE;
    epoch_key
}

fn block_execution_result_key(hash: &H256) -> Vec<u8> {
    append_suffix(hash, BLOCK_EXECUTION_RESULT_SUFFIX_BYTE)
}

fn block_reward_result_key(hash: &H256) -> Vec<u8> {
    append_suffix(hash, BLOCK_REWARD_RESULT_SUFFIX_BYTE)
}

fn epoch_execution_context_key(hash: &H256) -> Vec<u8> {
    append_suffix(hash, EPOCH_EXECUTION_CONTEXT_SUFFIX_BYTE)
}

fn epoch_consensus_epoch_execution_commitment_key(hash: &H256) -> Vec<u8> {
    append_suffix(hash, EPOCH_CONSENSUS_EXECUTION_INFO_SUFFIX_BYTE)
}

impl MallocSizeOf for DBManager {
    fn size_of(&self, ops: &mut MallocSizeOfOps) -> usize {
        // Here we only handle the case that all columns are stored within the
        // same ParityDB backend.
        self.table_db
            .get(&DBTable::Blocks)
            .expect("DBManager initialized")
            .size_of(ops)
    }
}
