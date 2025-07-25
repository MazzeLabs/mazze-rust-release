use crate::{
    block_data_manager::{
        db_decode_list, db_encode_list, BlamedHeaderVerifiedRoots,
        BlockExecutionResultWithEpoch, BlockTracesWithEpoch,
        CheckpointHashes, EpochExecutionContext,
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
    storage_db::KeyValueDbTrait, KvdbRocksdb, KvdbSqlite, KvdbSqliteStatements,
};
use mazze_types::H256;
use primitives::{Block, BlockHeader, SignedTransaction, TransactionIndex};
use rlp::Rlp;
use std::{collections::HashMap, fs, path::Path, sync::Arc};
use strum::IntoEnumIterator;
use strum_macros::EnumIter;

const LOCAL_BLOCK_INFO_SUFFIX_BYTE: u8 = 1;
const BLOCK_BODY_SUFFIX_BYTE: u8 = 2;
const BLOCK_EXECUTION_RESULT_SUFFIX_BYTE: u8 = 3;
const EPOCH_EXECUTION_CONTEXT_SUFFIX_BYTE: u8 = 4;
const EPOCH_CONSENSUS_EXECUTION_INFO_SUFFIX_BYTE: u8 = 5;
const EPOCH_EXECUTED_BLOCK_SET_SUFFIX_BYTE: u8 = 6;
const EPOCH_SKIPPED_BLOCK_SET_SUFFIX_BYTE: u8 = 7;
const BLOCK_TERMINAL_KEY: &[u8] = b"block_terminals";
const GC_PROGRESS_KEY: &[u8] = b"gc_progress";

#[derive(Clone, Copy, Hash, Ord, PartialOrd, Eq, PartialEq, EnumIter)]
enum DBTable {
    Misc,
    Blocks,
    Transactions,
    EpochNumbers,
    BlamedHeaderVerifiedRoots,
    BlockTraces,
    HashByBlockNumber,
}
// TODO: validate POS related columns have been successfully removed
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

fn sqlite_db_table(table: DBTable) -> String {
    match table {
        DBTable::Misc => "misc",
        DBTable::Blocks => "blocks",
        DBTable::Transactions => "transactions",
        DBTable::EpochNumbers => "epoch_numbers",
        DBTable::BlamedHeaderVerifiedRoots => "blamed_header_verified_roots",
        DBTable::BlockTraces => "block_traces",
        DBTable::HashByBlockNumber => "hash_by_block_number",
    }
    .into()
}

pub struct DBManager {
    table_db: HashMap<DBTable, Box<dyn KeyValueDbTrait<ValueType = Box<[u8]>>>>,
    pow: Arc<PowComputer>,
    genesis_hash: H256,
}

impl DBManager {
    pub fn new_from_rocksdb(
        db: Arc<SystemDB>, pow: Arc<PowComputer>, genesis_hash: H256,
    ) -> Self {
        let mut table_db = HashMap::new();

        for table in DBTable::iter() {
            table_db.insert(
                table,
                Box::new(KvdbRocksdb {
                    kvdb: db.key_value().clone(),
                    col: rocks_db_col(table),
                })
                    as Box<dyn KeyValueDbTrait<ValueType = Box<[u8]>>>,
            );
        }
        Self {
            table_db,
            pow,
            genesis_hash,
        }
    }
}

impl DBManager {
    pub fn new_from_sqlite(
        db_path: &Path, pow: Arc<PowComputer>, genesis_hash: H256,
    ) -> Self {
        if let Err(e) = fs::create_dir_all(db_path) {
            panic!("Error creating database directory: {:?}", e);
        }
        let mut table_db = HashMap::new();
        for table in DBTable::iter() {
            let table_str = sqlite_db_table(table);
            let (_, sqlite_db) = KvdbSqlite::open_or_create(
                &db_path.join(table_str.as_str()), /* Use separate database
                                                    * for
                                                    * different table */
                Arc::new(
                    KvdbSqliteStatements::make_statements(
                        &[&"value"],
                        &[&"BLOB"],
                        table_str.as_str(),
                        false,
                    )
                    .unwrap(),
                ),
                false, /* unsafe_mode */
            )
            .expect("Open sqlite failure");
            table_db.insert(
                table,
                Box::new(sqlite_db)
                    as Box<dyn KeyValueDbTrait<ValueType = Box<[u8]>>>,
            );
        }
        Self {
            table_db,
            pow,
            genesis_hash,
        }
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
    /// again. TODO Maybe we can use in-place modification (operator `merge`
    /// in rocksdb) to keep the info together with the block.
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

    pub fn block_execution_result_from_db(
        &self, hash: &H256,
    ) -> Option<BlockExecutionResultWithEpoch> {
        self.load_decodable_val(
            DBTable::Blocks,
            &block_execution_result_key(hash),
        )
    }

    pub fn remove_block_execution_result_from_db(&self, hash: &H256) {
        self.remove_from_db(DBTable::Blocks, &block_execution_result_key(hash))
    }

    pub fn remove_block_trace_from_db(&self, hash: &H256) {
        self.remove_from_db(DBTable::BlockTraces, hash.as_bytes())
    }

    pub fn remove_transaction_index_from_db(&self, hash: &H256) {
        self.remove_from_db(DBTable::Transactions, hash.as_bytes())
    }

    pub fn insert_checkpoint_hashes_to_db(
        &self, checkpoint_prev: &H256, checkpoint_cur: &H256,
    ) {
        self.insert_encodable_val(
            DBTable::Misc,
            b"checkpoint",
            &CheckpointHashes::new(*checkpoint_prev, *checkpoint_cur),
        );
    }

    pub fn checkpoint_hashes_from_db(&self) -> Option<(H256, H256)> {
        let checkpoints: CheckpointHashes =
            self.load_decodable_val(DBTable::Misc, b"checkpoint")?;
        Some((checkpoints.prev_hash, checkpoints.cur_hash))
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
        self.table_db
            .get(&table)
            .unwrap()
            .put(db_key, &value)
            .expect("db insertion failure");
    }

    fn remove_from_db(&self, table: DBTable, db_key: &[u8]) {
        self.table_db
            .get(&table)
            .unwrap()
            .delete(db_key)
            .expect("db removal failure");
    }

    fn load_from_db(&self, table: DBTable, db_key: &[u8]) -> Option<Box<[u8]>> {
        self.table_db
            .get(&table)
            .unwrap()
            .get(db_key)
            .expect("db read failure")
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
        Some(V::db_decode(&encoded).expect("decode succeeds"))
    }

    fn load_decodable_list<V>(
        &self, table: DBTable, db_key: &[u8],
    ) -> Option<Vec<V>>
    where
        V: DatabaseDecodable,
    {
        let encoded = self.load_from_db(table, db_key)?;
        Some(db_decode_list(&encoded).expect("decode succeeds"))
    }

    pub fn get_current_seed_hash(&self, epoch_height: u64) -> H256 {
        // Calculate the current epoch
        let current_epoch = epoch_height / RANDOMX_EPOCH_LENGTH;

        // For epoch 0, use genesis block
        if current_epoch == 0 {
            return self.genesis_hash;
        }

        // For all other epochs, use the block at the start of the previous epoch
        // floor((height - 1) / epoch_length) * epoch_length - epoch_length
        let seed_height = (current_epoch - 1) * RANDOMX_EPOCH_LENGTH;
        let seed_hash = self
            .executed_epoch_set_hashes_from_db(seed_height)
            .and_then(|hashes| hashes.last().cloned())
            .unwrap_or_default();
        trace!(
            "get_current_seed hash for epoch {}: {:?}",
            current_epoch,
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

fn epoch_execution_context_key(hash: &H256) -> Vec<u8> {
    append_suffix(hash, EPOCH_EXECUTION_CONTEXT_SUFFIX_BYTE)
}

fn epoch_consensus_epoch_execution_commitment_key(hash: &H256) -> Vec<u8> {
    append_suffix(hash, EPOCH_CONSENSUS_EXECUTION_INFO_SUFFIX_BYTE)
}

impl MallocSizeOf for DBManager {
    fn size_of(&self, ops: &mut MallocSizeOfOps) -> usize {
        // Here we only handle the case that all columns are stored within the
        // same rocksdb.
        self.table_db
            .get(&DBTable::Blocks)
            .expect("DBManager initialized")
            .size_of(ops)
    }
}
