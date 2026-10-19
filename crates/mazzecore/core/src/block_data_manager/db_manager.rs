use crate::{
    block_data_manager::{
        db_decode_list, db_encode_list, BlamedHeaderVerifiedRoots,
        BlockExecutionResultWithEpoch, BlockRewardResult, BlockTracesWithEpoch,
        CheckpointHashes, DataVersionTuple, EpochExecutionContext,
        LocalBlockInfo,
    },
    pow::PowComputer,
    verification::VerificationConfig,
};
use byteorder::{ByteOrder, LittleEndian};
use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};
use mazze_internal_common::{
    DatabaseDecodable, DatabaseEncodable, EpochExecutionCommitment,
};
use mazze_parameters::pow::RANDOMX_EPOCH_LENGTH;
use mazze_storage::{
    storage_db::{KeyValueDbTrait, KeyValueDbTraitRead},
    KvdbMdbx, MdbxColumn, MdbxEnv,
};
use mazze_types::H256;
use primitives::{
    Block, BlockHeader, SignedTransaction, TransactionIndex,
};
use metrics::{Counter, CounterUsize};
use rlp::Rlp;
use parking_lot::Mutex;
use std::{
    cell::Cell,
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
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

/// Sentinel router key for a bare 32-byte block-hash key in the
/// `Blocks` compound DBTable (BlockHeader writes). Chosen to be
/// `0` because the suffix bytes used by every other sub-table are
/// all in the range 1..=8 — no collision possible.
const BLOCKS_ROUTE_BARE_HEADER: u8 = 0;

/// Route a `DBTable::Blocks` write to a sub-mirror. Header writes
/// use the raw 32-byte block hash; every other sub-table appends
/// a single suffix byte to the hash. The router inspects length
/// first (headers are the only 32-byte-exact key shape) and falls
/// back to the last byte for the suffixed variants.
///
/// Preconditions the caller (DBManager) guarantees:
/// * All writes to `DBTable::Blocks` originate from one of the
///   `insert_*` helpers in this file, so the key shape is either
///   `H256::len_bytes()` or `H256::len_bytes() + 1`.
/// * `k` is never empty — a zero-length key is impossible on
///   this table.
fn blocks_route(k: &[u8]) -> u8 {
    if k.len() == H256::len_bytes() {
        BLOCKS_ROUTE_BARE_HEADER
    } else {
        *k.last().expect(
            "blocks_route: empty key — no DBTable::Blocks caller \
             produces this",
        )
    }
}

/// Route a `DBTable::EpochNumbers` write. Both sub-tables share a
/// 9-byte key shape (`epoch_u64_le | suffix_byte`), so routing is
/// just "look at the last byte". Executed / skipped are the only
/// two legal suffixes.
fn epoch_numbers_route(k: &[u8]) -> u8 {
    debug_assert_eq!(
        k.len(),
        9,
        "epoch_numbers_route: unexpected key length"
    );
    *k.last().expect("epoch_numbers_route: empty key")
}

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

impl DBTable {
    /// Stable name used in RPC arguments and JSON responses.
    /// Matches the variant identifier so `Debug` and this method
    /// stay in lockstep — renaming a variant requires updating
    /// both.
    pub fn name(self) -> &'static str {
        match self {
            DBTable::Misc => "Misc",
            DBTable::Blocks => "Blocks",
            DBTable::Transactions => "Transactions",
            DBTable::EpochNumbers => "EpochNumbers",
            DBTable::BlamedHeaderVerifiedRoots => {
                "BlamedHeaderVerifiedRoots"
            }
            DBTable::BlockTraces => "BlockTraces",
            DBTable::HashByBlockNumber => "HashByBlockNumber",
        }
    }

    /// Parse a table name from the RPC layer. Case-sensitive by
    /// design so operators use exactly the strings dashboards
    /// display; `Ok(None)` on unknown name so the RPC can return
    /// a clean error to the client.
    pub fn from_name(s: &str) -> Option<DBTable> {
        Some(match s {
            "Misc" => DBTable::Misc,
            "Blocks" => DBTable::Blocks,
            "Transactions" => DBTable::Transactions,
            "EpochNumbers" => DBTable::EpochNumbers,
            "BlamedHeaderVerifiedRoots" => {
                DBTable::BlamedHeaderVerifiedRoots
            }
            "BlockTraces" => DBTable::BlockTraces,
            "HashByBlockNumber" => DBTable::HashByBlockNumber,
            _ => return None,
        })
    }
}

/// Per-table backend metrics registered under the
/// `mdbx_db.<table_name>` group. Split by op (put / delete / get)
/// and outcome (ok / fail) so operator dashboards can:
///   - confirm the table is being exercised (put_ok / get_ok climb
///     as the executor commits epochs), and
///   - alert on any nonzero fail counter — a real durability or
///     env-corruption signal since there's no fallback.
///
/// Phase 5a rename: previously `mdbx_shadow.<col>` when the same
/// column was the shadow side of a paritydb mirror. Now the MDBX
/// column IS the store, so the group prefix is bare `mdbx_db`.
struct BackendMetrics {
    put_ok: Arc<dyn Counter<usize>>,
    put_fail: Arc<dyn Counter<usize>>,
    delete_ok: Arc<dyn Counter<usize>>,
    delete_fail: Arc<dyn Counter<usize>>,
}

impl BackendMetrics {
    fn new(table_name: &str) -> Self {
        let group = format!("mdbx_db.{}", table_name);
        Self {
            put_ok: CounterUsize::register_with_group(&group, "put_ok"),
            put_fail: CounterUsize::register_with_group(&group, "put_fail"),
            delete_ok: CounterUsize::register_with_group(
                &group,
                "delete_ok",
            ),
            delete_fail: CounterUsize::register_with_group(
                &group,
                "delete_fail",
            ),
        }
    }
}

/// A single-column MDBX-backed store — one [`KvdbMdbx`] wrapped
/// with metric bumping. Used for DBTables that map 1:1 to a
/// [`MdbxColumn`] and don't need key-suffix routing.
struct SimpleBackend {
    kvdb: KvdbMdbx,
    metrics: BackendMetrics,
}

impl SimpleBackend {
    fn put(&self, k: &[u8], v: &[u8]) -> mazze_storage::Result<()> {
        match self.kvdb.put(k, v) {
            Ok(_) => {
                self.metrics.put_ok.inc(1);
                Ok(())
            }
            Err(e) => {
                self.metrics.put_fail.inc(1);
                Err(e)
            }
        }
    }

    fn delete(&self, k: &[u8]) -> mazze_storage::Result<()> {
        match self.kvdb.delete(k) {
            Ok(_) => {
                self.metrics.delete_ok.inc(1);
                Ok(())
            }
            Err(e) => {
                self.metrics.delete_fail.inc(1);
                Err(e)
            }
        }
    }

    fn get(
        &self, k: &[u8],
    ) -> mazze_storage::Result<Option<Box<[u8]>>> {
        self.kvdb.get(k)
    }

    /// §B1: apply many puts/deletes in ONE MDBX rw_txn (one begin/commit,
    /// one grab of the global writer lock) instead of one transaction per
    /// put. Collapses e.g. a block's ~hundreds of tx-index writes into a
    /// single commit — the biggest write-amplification win under load.
    fn write_batch(
        &self, ops: &[mazze_storage::BatchOp],
    ) -> mazze_storage::Result<()> {
        match self.kvdb.write_batch(ops) {
            Ok(_) => {
                self.metrics.put_ok.inc(ops.len());
                Ok(())
            }
            Err(e) => {
                self.metrics.put_fail.inc(1);
                Err(e)
            }
        }
    }
}

/// A compound MDBX-backed store — one [`SimpleBackend`] per logical
/// sub-table, keyed by the byte the router returns for each write.
/// Under ParityDB these sub-tables used to be multiplexed into one
/// column via key-suffix bytes; Phase 5a split them so each
/// suffix lands in its own [`MdbxColumn`] on the write and read
/// paths alike (per-sub metrics, per-sub disk footprint, per-sub
/// prune granularity).
///
/// **Routing completeness invariant**: `route(k)` must return a
/// key present in `subs` for every possible write to the DBTable.
/// A missing sub-key is a programmer error (a silent drop would
/// lose data), so we panic loudly.
struct CompoundBackend {
    /// Sub-backends keyed by the `u8` the router returns.
    subs: HashMap<u8, SimpleBackend>,
    /// Key-inspection function. `fn` (not `Box<dyn Fn>`) so
    /// `CompoundBackend` stays trivially `Send + Sync` and the
    /// routing rule sits next to the DBTable it serves.
    route: fn(&[u8]) -> u8,
    /// Human-readable DBTable name — logged on the "missing
    /// sub-backend" panic so operators can grep for it.
    table_name: &'static str,
}

impl CompoundBackend {
    fn dispatch<'a>(&'a self, key: &[u8]) -> &'a SimpleBackend {
        let sub_key = (self.route)(key);
        self.subs.get(&sub_key).unwrap_or_else(|| {
            panic!(
                "CompoundBackend routing hole: table={} key_len={} \
                 route={:#x} — no sub-backend registered. This is a \
                 programmer bug in the router or a novel key shape; \
                 adding a sub-backend keeps the write path complete.",
                self.table_name,
                key.len(),
                sub_key
            )
        })
    }

    fn put(&self, k: &[u8], v: &[u8]) -> mazze_storage::Result<()> {
        self.dispatch(k).put(k, v)
    }

    fn delete(&self, k: &[u8]) -> mazze_storage::Result<()> {
        self.dispatch(k).delete(k)
    }

    fn get(
        &self, k: &[u8],
    ) -> mazze_storage::Result<Option<Box<[u8]>>> {
        self.dispatch(k).get(k)
    }

    /// §B1: batched write. Ops fan out to sub-backends by the router, then
    /// each sub commits its slice in a single rw_txn. A batch spanning subs
    /// is not globally atomic across columns, but callers batch a single
    /// logical unit (e.g. one block's tx-index) that routes to one sub, so
    /// in practice each batch lands in a single sub / one commit.
    fn write_batch(
        &self, ops: &[mazze_storage::BatchOp],
    ) -> mazze_storage::Result<()> {
        use mazze_storage::BatchOp;
        let mut by_sub: HashMap<u8, Vec<BatchOp>> = HashMap::new();
        for op in ops {
            let (route_key, cloned) = match op {
                BatchOp::Put(k, v) => ((self.route)(*k), BatchOp::Put(*k, *v)),
                BatchOp::Delete(k) => ((self.route)(*k), BatchOp::Delete(*k)),
            };
            by_sub.entry(route_key).or_default().push(cloned);
        }
        for (sub_key, sub_ops) in &by_sub {
            self.subs
                .get(sub_key)
                .unwrap_or_else(|| {
                    panic!(
                        "CompoundBackend batch routing hole: table={} \
                         route={:#x} — no sub-backend registered",
                        self.table_name, sub_key
                    )
                })
                .write_batch(sub_ops)?;
        }
        Ok(())
    }
}

/// One DBTable's live storage. `Simple` for 1:1 mappings to a
/// `MdbxColumn`; `Compound` for tables whose keyspace fans out to
/// multiple columns via a router.
enum TableBackend {
    Simple(SimpleBackend),
    Compound(CompoundBackend),
}

impl TableBackend {
    fn put(&self, k: &[u8], v: &[u8]) -> mazze_storage::Result<()> {
        match self {
            TableBackend::Simple(b) => b.put(k, v),
            TableBackend::Compound(c) => c.put(k, v),
        }
    }

    /// §B1: batched write, dispatched to the concrete backend.
    fn write_batch(
        &self, ops: &[mazze_storage::BatchOp],
    ) -> mazze_storage::Result<()> {
        match self {
            TableBackend::Simple(b) => b.write_batch(ops),
            TableBackend::Compound(c) => c.write_batch(ops),
        }
    }

    fn delete(&self, k: &[u8]) -> mazze_storage::Result<()> {
        match self {
            TableBackend::Simple(b) => b.delete(k),
            TableBackend::Compound(c) => c.delete(k),
        }
    }

    fn get(
        &self, k: &[u8],
    ) -> mazze_storage::Result<Option<Box<[u8]>>> {
        match self {
            TableBackend::Simple(b) => b.get(k),
            TableBackend::Compound(c) => c.get(k),
        }
    }

    /// The single-column backend a key of this table lives in.
    fn column_of(&self, k: &[u8]) -> &SimpleBackend {
        match self {
            TableBackend::Simple(b) => b,
            TableBackend::Compound(c) => c.dispatch(k),
        }
    }
}

/// See [`DBManager::buffer_writes`].
pub struct WriteBufferGuard<'a>(Option<&'a DBManager>);

impl Drop for WriteBufferGuard<'_> {
    fn drop(&mut self) {
        if let Some(db) = self.0 {
            BUFFERING_FOR.with(|b| b.set(0));
            db.flush_write_buffer();
        }
    }
}

thread_local! {
    /// Address of the `DBManager` whose writes this thread currently
    /// buffers (see `DBManager::with_write_buffer`), or 0.
    static BUFFERING_FOR: Cell<usize> = Cell::new(0);
}

pub struct DBManager {
    /// Per-`DBTable` MDBX-native store. `Simple` for tables that
    /// map 1:1 to a `MdbxColumn`; `Compound` for tables that fan
    /// out to multiple columns via a key router. This is the ONLY
    /// storage this manager talks to — no ParityDB anywhere on the
    /// path.
    table_backends: HashMap<DBTable, TableBackend>,
    pow: Arc<PowComputer>,
    genesis_hash: H256,
    /// The env every table lives in (for multi-column atomic flushes).
    env: Arc<MdbxEnv>,
    /// §B1 write-combining: writes made inside `with_write_buffer` on the
    /// buffering thread, `None` = delete. Visible to every reader until
    /// flushed together in one MDBX transaction.
    write_buffer: Mutex<HashMap<(DBTable, Vec<u8>), Option<Vec<u8>>>>,
    /// `write_buffer.len()`, readable without the lock (readers skip the
    /// buffer while it is empty).
    buffered: AtomicUsize,
}

impl DBManager {
    /// Build the per-`DBTable` MDBX backend map.
    ///
    /// Simple tables map to a single `MdbxColumn`; compound tables
    /// (Blocks, EpochNumbers) fan out to multiple columns keyed by
    /// the byte a router extracts from every write. Compound
    /// routing replaces the ParityDB-era practice of multiplexing
    /// sub-tables into one column via key-suffix bytes.
    fn build_table_backends(
        env: &Arc<MdbxEnv>,
    ) -> HashMap<DBTable, TableBackend> {
        let make_simple = |col: MdbxColumn| -> SimpleBackend {
            SimpleBackend {
                kvdb: KvdbMdbx::with_column(
                    Arc::clone(env),
                    col.id(),
                ),
                metrics: BackendMetrics::new(col.name()),
            }
        };
        let make_simple_entry =
            |col: MdbxColumn| -> TableBackend {
                TableBackend::Simple(make_simple(col))
            };

        let mut backends: HashMap<DBTable, TableBackend> = HashMap::new();

        // Simple 1:1 mappings.
        backends.insert(
            DBTable::HashByBlockNumber,
            make_simple_entry(MdbxColumn::HashByNumber),
        );
        backends.insert(
            DBTable::Transactions,
            make_simple_entry(MdbxColumn::TxIndex),
        );
        backends.insert(
            DBTable::BlamedHeaderVerifiedRoots,
            make_simple_entry(MdbxColumn::BlamedHeaderVerifiedRoots),
        );
        backends.insert(
            DBTable::BlockTraces,
            make_simple_entry(MdbxColumn::BlockTraces),
        );
        backends.insert(
            DBTable::Misc,
            make_simple_entry(MdbxColumn::Misc),
        );

        // Compound: Blocks (7 sub-columns keyed by suffix byte,
        // with the bare 32B key routing to `BlockHeaders`).
        {
            let mut subs: HashMap<u8, SimpleBackend> = HashMap::new();
            subs.insert(
                BLOCKS_ROUTE_BARE_HEADER,
                make_simple(MdbxColumn::BlockHeaders),
            );
            subs.insert(
                LOCAL_BLOCK_INFO_SUFFIX_BYTE,
                make_simple(MdbxColumn::LocalBlockInfo),
            );
            subs.insert(
                BLOCK_BODY_SUFFIX_BYTE,
                make_simple(MdbxColumn::BlockBodies),
            );
            subs.insert(
                BLOCK_EXECUTION_RESULT_SUFFIX_BYTE,
                make_simple(MdbxColumn::BlockExecutionResult),
            );
            subs.insert(
                EPOCH_EXECUTION_CONTEXT_SUFFIX_BYTE,
                make_simple(MdbxColumn::EpochExecutionContext),
            );
            subs.insert(
                EPOCH_CONSENSUS_EXECUTION_INFO_SUFFIX_BYTE,
                make_simple(MdbxColumn::BlockExecutionCommitment),
            );
            subs.insert(
                BLOCK_REWARD_RESULT_SUFFIX_BYTE,
                make_simple(MdbxColumn::BlockRewards),
            );
            backends.insert(
                DBTable::Blocks,
                TableBackend::Compound(CompoundBackend {
                    subs,
                    route: blocks_route,
                    table_name: "Blocks",
                }),
            );
        }

        // Compound: EpochNumbers (executed + skipped).
        {
            let mut subs: HashMap<u8, SimpleBackend> = HashMap::new();
            subs.insert(
                EPOCH_EXECUTED_BLOCK_SET_SUFFIX_BYTE,
                make_simple(MdbxColumn::EpochBlocks),
            );
            subs.insert(
                EPOCH_SKIPPED_BLOCK_SET_SUFFIX_BYTE,
                make_simple(MdbxColumn::EpochSkippedBlockSet),
            );
            backends.insert(
                DBTable::EpochNumbers,
                TableBackend::Compound(CompoundBackend {
                    subs,
                    route: epoch_numbers_route,
                    table_name: "EpochNumbers",
                }),
            );
        }

        backends
    }

    /// Build a `DBManager` on top of the shared MDBX env. This is
    /// the only constructor after Phase 5a — no more paritydb
    /// backend, no more shadow-mirror machinery.
    pub fn new(
        env: Arc<MdbxEnv>, pow: Arc<PowComputer>, genesis_hash: H256,
    ) -> Self {
        Self {
            table_backends: Self::build_table_backends(&env),
            pow,
            genesis_hash,
            env,
            write_buffer: Mutex::new(HashMap::new()),
            buffered: AtomicUsize::new(0),
        }
    }

    /// Load the persisted true genesis block (the hash stored at block
    /// number 0, see `BlockDataManager::new`) straight from the MDBX env,
    /// before a `DBManager`/`PowComputer` exists. Used at startup so a
    /// restarted node keeps the genesis it was created with instead of
    /// recomputing one (with `execute_genesis = false` the recomputed
    /// genesis skips the genesis txs and gets a different hash).
    pub fn genesis_block_from_env(env: &Arc<MdbxEnv>) -> Option<Block> {
        let backends = Self::build_table_backends(env);
        let get = |table: DBTable, key: &[u8]| -> Option<Box<[u8]>> {
            backends.get(&table)?.get(key).ok().flatten()
        };
        let hash = H256::db_decode(&get(
            DBTable::HashByBlockNumber,
            &0u64.to_be_bytes(),
        )?)
        .ok()?;
        let header =
            BlockHeader::db_decode(&get(DBTable::Blocks, hash.as_bytes())?)
                .ok()?;
        let body = get(DBTable::Blocks, &block_body_key(&hash))?;
        let transactions =
            Block::decode_body_with_tx_public(&Rlp::new(&body)).ok()?;
        let block = Block::new(header, transactions);
        (block.hash() == hash).then_some(block)
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

    /// Main-chain block at `height` (a multiple of `RANDOMX_EPOCH_LENGTH`)
    /// below a fast-sync anchor, taken from the anchor's authenticated
    /// parent chain. It is the RandomX seed of the epochs above it, which a
    /// node anchored there never orders itself (no epoch set is stored for
    /// heights below the anchor). See docs/fast-sync-design.md §5.19.
    pub fn insert_anchored_seed_hash_to_db(&self, height: u64, hash: &H256) {
        self.insert_encodable_val(
            DBTable::Misc,
            &anchored_seed_key(height),
            hash,
        );
    }

    pub fn anchored_seed_hash_from_db(&self, height: u64) -> Option<H256> {
        self.load_decodable_val(DBTable::Misc, &anchored_seed_key(height))
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

    /// §B1 write-combining. Run `f` with every write this thread makes
    /// through this `DBManager` buffered in memory, then commit them all
    /// in ONE MDBX transaction (instead of one transaction per key).
    ///
    /// While buffered, the writes are visible to every reader (reads check
    /// the buffer first). A write of the same key by another thread drops
    /// the buffered one (the later write wins, as without buffering). A
    /// crash before the flush loses the buffered writes together, as if
    /// it had happened before the first of them: callers must not let
    /// another component act on a buffered write (persist something that
    /// refers to it) before this returns. Nested calls join the outer
    /// buffer.
    pub fn with_write_buffer<T>(&self, f: impl FnOnce() -> T) -> T {
        let _flush = self.buffer_writes();
        f()
    }

    /// Guard form of [`Self::with_write_buffer`]: buffering ends and the
    /// buffer is flushed when the guard is dropped.
    pub fn buffer_writes(&self) -> WriteBufferGuard<'_> {
        let me = self as *const Self as usize;
        let prev = BUFFERING_FOR.with(|b| b.get());
        if prev == me {
            return WriteBufferGuard(None);
        }
        // Another DBManager buffering on this thread (tests only): flush
        // nothing of it here, just don't capture its writes.
        if prev != 0 {
            return WriteBufferGuard(None);
        }
        BUFFERING_FOR.with(|b| b.set(me));
        WriteBufferGuard(Some(self))
    }

    /// Commit the buffered writes in one MDBX transaction. The lock is
    /// held until the commit is done, so a reader never misses a value
    /// that left the buffer but is not committed yet.
    fn flush_write_buffer(&self) {
        let mut buffer = self.write_buffer.lock();
        if buffer.is_empty() {
            return;
        }
        let mut ops: Vec<(u32, mazze_storage::BatchOp)> =
            Vec::with_capacity(buffer.len());
        for ((table, key), value) in buffer.iter() {
            let col = self.table_backends[table].column_of(key);
            col.metrics.put_ok.inc(1);
            ops.push((
                col.kvdb.col,
                match value {
                    Some(v) => mazze_storage::BatchOp::Put(key, v),
                    None => mazze_storage::BatchOp::Delete(key),
                },
            ));
        }
        if let Err(e) = self.env.write_multi(&ops) {
            DB_WRITE_FAILURES.inc(ops.len());
            error!(
                "db buffered write of {} key(s) failed: {:?}. CACHE/DB \
                 DIVERGENCE",
                ops.len(),
                e
            );
        }
        drop(ops);
        buffer.clear();
        self.buffered.store(0, Ordering::Release);
    }

    /// Route a write: buffered (returns `None`) if this thread is inside
    /// `with_write_buffer`, otherwise handed back (`Some(value)`) for an
    /// immediate write after dropping any buffered write of the same key.
    fn buffer_write(
        &self, table: DBTable, db_key: &[u8], value: Option<Vec<u8>>,
    ) -> Option<Option<Vec<u8>>> {
        let me = self as *const Self as usize;
        if BUFFERING_FOR.with(|b| b.get()) == me {
            let mut buffer = self.write_buffer.lock();
            buffer.insert((table, db_key.to_vec()), value);
            self.buffered.store(buffer.len(), Ordering::Release);
            return None;
        }
        if self.buffered.load(Ordering::Acquire) > 0 {
            let mut buffer = self.write_buffer.lock();
            if buffer.remove(&(table, db_key.to_vec())).is_some() {
                self.buffered.store(buffer.len(), Ordering::Release);
            }
        }
        Some(value)
    }

    /// The functions below are private utils used by the DBManager to access
    /// database
    fn insert_to_db(&self, table: DBTable, db_key: &[u8], value: Vec<u8>) {
        let value = match self.buffer_write(table, db_key, Some(value)) {
            None => return,
            Some(v) => v.expect("buffer_write hands back the value"),
        };
        let backend = self.table_backends.get(&table).expect(
            "DBManager: no backend registered for the DBTable — \
             every variant is wired in build_table_backends",
        );
        if let Err(e) = backend.put(db_key, &value) {
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

    /// §B1: batch many `(key, value)` puts for one table into a SINGLE MDBX
    /// commit (via `write_batch`) instead of one commit per put. The caller
    /// owns the buffers; we borrow them to build the ops. Same
    /// CACHE/DB-divergence logging as `insert_to_db` on failure (no panic).
    /// Intended for the executor's per-block tx-index / per-epoch block-data
    /// writes, which otherwise do hundreds of separate commits under load.
    pub fn insert_batch_to_db(
        &self, table: DBTable, pairs: &[(Vec<u8>, Vec<u8>)],
    ) {
        if pairs.is_empty() {
            return;
        }
        let backend = self.table_backends.get(&table).expect(
            "DBManager: no backend registered for the DBTable",
        );
        let ops: Vec<mazze_storage::BatchOp> = pairs
            .iter()
            .map(|(k, v)| {
                mazze_storage::BatchOp::Put(k.as_slice(), v.as_slice())
            })
            .collect();
        if let Err(e) = backend.write_batch(&ops) {
            DB_WRITE_FAILURES.inc(1);
            error!(
                "db batch insertion failed for table {:?} n_ops={}: {:?}. \
                 CACHE/DB DIVERGENCE",
                table,
                ops.len(),
                e
            );
        }
    }

    fn remove_from_db(&self, table: DBTable, db_key: &[u8]) {
        if self.buffer_write(table, db_key, None).is_none() {
            return;
        }
        let backend = self.table_backends.get(&table).expect(
            "DBManager: no backend registered for the DBTable",
        );
        backend.delete(db_key).expect("db removal failure");
    }

    fn load_from_db(&self, table: DBTable, db_key: &[u8]) -> Option<Box<[u8]>> {
        if self.buffered.load(Ordering::Acquire) > 0 {
            let buffer = self.write_buffer.lock();
            if let Some(v) = buffer.get(&(table, db_key.to_vec())) {
                return v.as_ref().map(|v| v.clone().into_boxed_slice());
            }
        }
        let backend = self.table_backends.get(&table).expect(
            "DBManager: no backend registered for the DBTable",
        );
        match backend.get(db_key) {
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
            .and_then(|hashes| hashes.last().cloned())
            .or_else(|| self.anchored_seed_hash_from_db(seed_height));
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

fn anchored_seed_key(height: u64) -> Vec<u8> {
    let mut key = b"anchored_seed".to_vec();
    key.extend_from_slice(&height.to_le_bytes());
    key
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
    fn size_of(&self, _ops: &mut MallocSizeOfOps) -> usize {
        // MDBX-native: the store's memory is memory-mapped from
        // disk, not owned Rust heap. Reporting mmap size here is
        // misleading (doesn't reflect resident set) and there are
        // no other significant Rust allocations on DBManager to
        // account for. Return 0.
        0
    }
}

#[cfg(test)]
mod write_buffer_tests {
    use super::*;

    fn make_db() -> (tempfile::TempDir, Arc<DBManager>) {
        let dir = tempfile::Builder::new().prefix("db_manager_write_buffer").tempdir().unwrap();
        let env = MdbxEnv::open(dir.path()).unwrap();
        let db = DBManager::new(
            env,
            Arc::new(PowComputer::new(H256::zero())),
            H256::zero(),
        );
        (dir, Arc::new(db))
    }

    /// Reads straight from MDBX, bypassing the buffer.
    fn committed(db: &DBManager, n: u64) -> Option<H256> {
        db.table_backends[&DBTable::HashByBlockNumber]
            .get(&n.to_be_bytes())
            .unwrap()
            .map(|v| H256::db_decode(&v).unwrap())
    }

    #[test]
    fn buffered_writes_are_visible_and_flushed_together() {
        let (_dir, db) = make_db();
        let (h1, h2) = (H256::from_low_u64_be(1), H256::from_low_u64_be(2));
        db.insert_hash_by_block_number_to_db(9, &h1);
        db.with_write_buffer(|| {
            db.insert_hash_by_block_number_to_db(1, &h1);
            db.insert_hash_by_block_number_to_db(2, &h1);
            db.insert_hash_by_block_number_to_db(2, &h2);
            db.remove_from_db(
                DBTable::HashByBlockNumber,
                &9u64.to_be_bytes(),
            );
            // Not committed yet ...
            assert_eq!(committed(&db, 1), None);
            assert_eq!(committed(&db, 9), Some(h1));
            // ... but visible to this and to other threads.
            assert_eq!(db.hash_by_block_number_from_db(&2), Some(h2));
            assert_eq!(db.hash_by_block_number_from_db(&9), None);
            let other = Arc::clone(&db);
            let seen = std::thread::spawn(move || {
                (
                    other.hash_by_block_number_from_db(&1),
                    other.hash_by_block_number_from_db(&9),
                )
            })
            .join()
            .unwrap();
            assert_eq!(seen, (Some(h1), None));
            // Nested scopes join the outer buffer.
            db.with_write_buffer(|| {
                db.insert_hash_by_block_number_to_db(3, &h2)
            });
            assert_eq!(committed(&db, 3), None);
        });
        assert_eq!(committed(&db, 1), Some(h1));
        assert_eq!(committed(&db, 2), Some(h2));
        assert_eq!(committed(&db, 3), Some(h2));
        assert_eq!(committed(&db, 9), None);
        assert_eq!(db.buffered.load(Ordering::Relaxed), 0);
        // Outside a scope writes are immediate.
        db.insert_hash_by_block_number_to_db(4, &h1);
        assert_eq!(committed(&db, 4), Some(h1));
    }

    #[test]
    fn direct_write_from_another_thread_wins_over_buffered_one() {
        let (_dir, db) = make_db();
        let (h1, h2) = (H256::from_low_u64_be(1), H256::from_low_u64_be(2));
        let guard = db.buffer_writes();
        db.insert_hash_by_block_number_to_db(5, &h1);
        let other = Arc::clone(&db);
        std::thread::spawn(move || {
            other.insert_hash_by_block_number_to_db(5, &h2)
        })
        .join()
        .unwrap();
        assert_eq!(committed(&db, 5), Some(h2));
        assert_eq!(db.hash_by_block_number_from_db(&5), Some(h2));
        drop(guard);
        assert_eq!(committed(&db, 5), Some(h2));
    }
}

