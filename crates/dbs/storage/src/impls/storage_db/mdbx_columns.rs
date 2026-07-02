// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! MDBX column catalog for Mazze storage (Phase 1 step 6).
//!
//! This is the authoritative list of MDBX sub-tables that the hot tier
//! will host after the ParityDB → MDBX migration. Each variant of
//! [`Column`] maps to exactly one MDBX sub-table under a shared
//! [`MdbxEnv`](super::kvdb_mdbx::MdbxEnv), addressed by the `u32` value
//! returned by [`Column::id`].
//!
//! # Why one column per logical table
//!
//! The current ParityDB layout collapses many logical tables into a
//! handful of "columns" (7 total) and disambiguates them via key
//! prefixes. That's dense but hostile to observability — per-column
//! [`KvdbMdbxStats`](super::kvdb_mdbx::KvdbMdbxStats) don't tell you
//! whether the leaf-page growth came from receipts or from state
//! trie nodes. With one column per table:
//!
//! - stats/metrics answer real questions ("how many bytes do receipts
//!   take?", "how full is the account history bitmap?"),
//! - writes to a table can be batched inside a single MDBX rw_txn
//!   without accidentally touching an unrelated table's pages,
//! - schema-level pruning (e.g. drop old traces) is `drop_table` on
//!   one column, not a full-column iterate-and-filter.
//!
//! Column ids are stable — new tables get new ids at the end.
//! Renaming a variant does not change on-disk layout as long as
//! [`Column::id`] stays the same.
//!
//! # Categories
//!
//! 1. **Chain (DAG)** — block headers, bodies, receipts, traces, and
//!    every derived index required to rebuild the DAG at startup.
//!    Preserves every DAG invariant enumerated in
//!    [storage-design.md](../../../../../docs/internal/storage-design.md)
//!    §4a. (16 columns.)
//! 2. **State (flat)** — the account/storage KV that replaces the
//!    MPT for the hot read/write path. MPT roots are still computed
//!    on demand from this data for header commitments. (4 columns.)
//! 3. **History (change indices)** — the Reth-style change-set +
//!    bitmap structure that answers "state of X at epoch E" without
//!    trie walks (§6.3). (7 columns.)
//! 4. **Consensus state** — chain metadata, snapshots, force-confirm
//!    decisions, RandomX seed table. (5 columns.)
//! 5. **Shielded** — Merkle frontier, root history, nullifier set,
//!    and verifying-key material for the shielded pool. Once
//!    [privacy-space-vs-native.md](../../../../../docs/internal/privacy-space-vs-native.md)
//!    (Monero-in-a-space) lands these move under `Space::Shielded`
//!    with different key prefixes — the column list here does not
//!    presume that split. (5 columns.)
//! 6. **Voluntary DA** — the off-consensus era-archive attestation
//!    gossip records (§10 Q4). (2 columns.)
//! 7. **Static-file coordination** (Phase 4) — tracks which block
//!    ranges have been offloaded from MDBX to `bodies/N-M.seg` /
//!    `receipts/N-M.seg` / `traces/N-M.seg` cold static files and
//!    which have been pruned outright. (2 columns.)
//! 8. **Migration / schema** (Phase 7) — version stamp for the
//!    on-disk layout, resumable migration cursor for ParityDB →
//!    MDBX, and periodic integrity checksums for corruption
//!    recovery. (3 columns.)
//! 9. **RPC log-query acceleration** — per-epoch bloom filters and
//!    address/topic → epoch-bitmap indices so `eth_getLogs` /
//!    `mazze_getLogs` can skip whole epochs without opening any
//!    receipt. (3 columns.)
//! 10. **Executor pins** — reward window metadata making the
//!     executor's `pending_execution_count` precise, unblocking the
//!     bounded-channel backpressure work (§Q7a). (1 column.)
//! 11. **Genesis / boot verification** — genesis summary written at
//!     boot so relaunches detect config drift instead of forking
//!     silently. (1 column.)
//!
//! **Total: 49 columns.** MDBX env is opened with
//! `set_max_tables(64)`, leaving ~15 slots for post-launch extensions.

/// Every logical table the storage layer knows about. Each variant is
/// backed by one MDBX sub-table, addressed at [`KvdbMdbx`](super::
/// kvdb_mdbx::KvdbMdbx) construction time via [`Column::id`].
///
/// **Stable ids** — never renumber existing variants; add new tables
/// at the end.
///
/// The `#[repr(u32)]` guarantees `variant as u32 == Column::id(&variant)`
/// so callers can inline the id via `Column::PlainAccount as u32`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Column {
    // ------------------------------------------------------------------
    // 1. Chain — DAG blocks + derived indices
    // ------------------------------------------------------------------
    //
    /// **`BlockHeaders`** — `block_hash (32B) → Header (RLP)`.
    ///
    /// Every block's header, keyed by hash. Storage-agnostic: we can
    /// rebuild the DAG's in-memory arena solely from headers because
    /// every parent + referee relationship lives in the header.
    ///
    /// Retention: full history on archive nodes; window on non-archive
    /// (see §4a.6). Hot on read; write-once per block.
    BlockHeaders = 0,

    /// **`BlockBodies`** — `block_hash (32B) → Body (RLP: Vec<Tx>)`.
    ///
    /// Transaction payloads. Cold-ish after execution (nobody reads a
    /// body once the epoch is executed unless via RPC or during peer
    /// serving). Migrates to `bodies/*.seg` static files past the
    /// retention window for full/full-fast nodes.
    BlockBodies = 1,

    /// **`BlockReceipts`** — `block_hash (32B) → Vec<Receipt> (RLP)`.
    ///
    /// Execution receipts (logs, cumulative gas, status). Read on RPC
    /// (`eth_getTransactionReceipt`), written once at execution commit.
    BlockReceipts = 2,

    /// **`BlockTraces`** — `block_hash (32B) → Vec<Trace> (RLP)`.
    ///
    /// Per-block execution traces. Optional: only written when the
    /// node config enables tracing (miner and full-fast typically
    /// disable). Cold, bulky (largest per-block artifact after body).
    BlockTraces = 3,

    /// **`BlockExecutionCommitment`** —
    /// `epoch_id (32B) → StateRootWithAuxInfo`.
    ///
    /// Executor watermark and state-root commitments per epoch.
    /// Required for header validation of downstream blocks that
    /// commit a state root DEFERRED_STATE_EPOCH_COUNT epochs back.
    BlockExecutionCommitment = 4,

    /// **`BlockRewards`** —
    /// `block_hash (32B) → RewardInfo { author, block_reward, tx_fees }`.
    ///
    /// Per-block reward accounting. Used by reward-audit RPCs and
    /// by the executor's reward-window pinning logic.
    BlockRewards = 5,

    /// **`TxIndex`** —
    /// `tx_hash (32B) → { block_hash (32B), index (u32) }`.
    ///
    /// Reverse lookup for `eth_getTransactionByHash` /
    /// `mazze_getTransactionByHash`. Grows linearly with tx history;
    /// optional on nodes with `persist_tx_index = false` (miners).
    TxIndex = 6,

    /// **`HashByNumber`** — `block_number (u64 BE) → block_hash (32B)`.
    ///
    /// Canonical mapping from a pivot-chain block number to its hash.
    /// Rewritten on reorg for numbers ≥ fork point.
    HashByNumber = 7,

    /// **`NumberByHash`** — `block_hash (32B) → block_number (u64 BE)`.
    ///
    /// Reverse of [`Column::HashByNumber`]. Optional (`persist_block_
    /// number_index = false` on miners); needed for RPCs that accept
    /// either a hash or a number.
    NumberByHash = 8,

    /// **`EpochBlocks`** — `epoch (u64 BE) → Vec<block_hash> (RLP)`.
    ///
    /// All blocks that landed in an epoch (pivot + referees), in
    /// `ordered_executable_epoch_blocks` order. This is the
    /// authoritative record of DAG topology per epoch — matches the
    /// invariant enumerated in storage-design §4a.4.
    EpochBlocks = 9,

    /// **`BestPivot`** — `epoch (u64 BE) → block_hash (32B)`.
    ///
    /// GHAST pivot chain: the heaviest block chosen as pivot at each
    /// epoch. Rewritten under reorg for epochs ≥ fork point (rare
    /// under force-confirm).
    BestPivot = 10,

    /// **`TimerChain`** — `timer_height (u64 BE) → block_hash (32B)`.
    ///
    /// The RandomX-anchored spine used for finality. Blocks passing
    /// `TIMER_CHAIN_BLOCK_DIFFICULTY_RATIO` are appended here; the
    /// force-confirm gate reads it every `TIMER_CHAIN_BEACON_INTERVAL`.
    TimerChain = 11,

    /// **`BlockIsTimer`** — `block_hash (32B) → bool (1B)`.
    ///
    /// O(1) lookup for "is this block on the timer chain?". Materialised
    /// index of the inverse of `TimerChain`; avoids a linear scan on
    /// consensus decisions.
    BlockIsTimer = 12,

    /// **`ChildrenByParent`** —
    /// `parent_hash (32B) → Vec<block_hash> (RLP)`.
    ///
    /// DAG children index. Reconstruction aid on startup; strictly a
    /// cache of the relationship implicit in every header's parent
    /// field. Rebuildable at any time by walking `BlockHeaders`.
    ChildrenByParent = 13,

    /// **`BlamedHeaderVerifiedRoots`** —
    /// `block_hash (32B) → VerifiedRootInfo (RLP)`.
    ///
    /// Records header-blame proof verification results for light-node
    /// support. Kept for compatibility with the current light-protocol
    /// handler; may be dropped if light-client support is deprecated.
    BlamedHeaderVerifiedRoots = 14,

    /// **`VerifiedInvalidBlocks`** —
    /// `block_hash (32B) → () (empty value)`.
    ///
    /// Set-typed table: an entry means "we have proven this block
    /// invalid, don't re-download or re-verify". Prevents peers from
    /// re-serving known-bad blocks in a request loop.
    VerifiedInvalidBlocks = 15,

    // ------------------------------------------------------------------
    // 2. State — flat KV (Reth-style, replaces MPT hot path)
    // ------------------------------------------------------------------

    /// **`PlainAccount`** —
    /// `(space (1B), address (20B)) → Account (RLP)`.
    ///
    /// The primary account KV: nonce, balance, storage_root
    /// (on-demand), code_hash. Replaces the MPT descent for account
    /// reads on the executor's hot path. One entry per address per
    /// space (native vs eSpace).
    ///
    /// `Account.code` is NOT inlined; use `code_hash` → [`CodeStore`].
    PlainAccount = 16,

    /// **`PlainStorage`** —
    /// `(space (1B), address (20B), key (32B)) → U256 (32B BE)`.
    ///
    /// Contract storage slot values. Flat KV — one entry per
    /// (address, slot) pair per space. Replaces the storage-MPT
    /// descent for `SLOAD` on the executor's hot path.
    PlainStorage = 17,

    /// **`CodeStore`** — `code_hash (32B) → code_bytes (varlen)`.
    ///
    /// Contract bytecode, deduplicated by hash. Many accounts share
    /// code (ERC-20 clones, factory-deployed proxies) — a shared table
    /// keyed by `code_hash` saves considerable storage.
    CodeStore = 18,

    /// **`StorageRoots`** — `(space (1B), address (20B)) → H256 (32B)`.
    ///
    /// Per-account storage-root hashes. Computed lazily when needed
    /// for MPT root reconstruction at header-commit time; cached here
    /// so subsequent reads don't re-hash the whole storage trie.
    StorageRoots = 19,

    // ------------------------------------------------------------------
    // 3. History — change indices (Reth-style)
    // ------------------------------------------------------------------

    /// **`AccountHistory`** —
    /// `(space (1B), address (20B)) → RoaringBitmap (varlen)`.
    ///
    /// For every account, the set of epochs where it was touched. The
    /// bitmap is Roaring-compressed. Answers "which epochs modified
    /// this account?" in O(bitmap-size) — the first step of a
    /// historical `getBalanceAt(address, block)` RPC.
    AccountHistory = 20,

    /// **`StorageHistory`** —
    /// `(space (1B), address (20B), key (32B)) → RoaringBitmap`.
    ///
    /// Same shape as [`AccountHistory`] but per storage slot. Answers
    /// "which epochs modified this slot?" for historical `getStorageAt`.
    StorageHistory = 21,

    /// **`AccountChangeSet`** —
    /// `(space (1B), address (20B), epoch (u64 BE)) →
    ///  { prev_account (RLP), source_block_hash (32B) }`.
    ///
    /// The undo-log: for every (account, epoch) pair where the
    /// account was touched, records the account state BEFORE that
    /// epoch's changes plus the block hash that caused the change.
    /// Reorg inversion filters by `source_block_hash IN reorged`;
    /// historical queries walk backward from current state.
    ///
    /// Per §10 Q5 the primary key is `(address, epoch)` with
    /// `source_block_hash` as secondary metadata — chosen because
    /// force-confirm makes reorg-precision less critical than
    /// epoch-precision.
    AccountChangeSet = 22,

    /// **`StorageChangeSet`** —
    /// `(space (1B), address (20B), key (32B), epoch (u64 BE)) →
    ///  { prev_value (32B), source_block_hash (32B) }`.
    ///
    /// Storage-slot equivalent of [`AccountChangeSet`]. Same semantic
    /// invariants.
    StorageChangeSet = 23,

    /// **`AccountsAtEpoch`** —
    /// `epoch (u64 BE) → Vec<(space, address)> (RLP)`.
    ///
    /// The inverse index: for every epoch, the list of touched
    /// accounts. Used for era-archive artifact generation (dump every
    /// account modified in the era) and for reorg detection (compare
    /// which accounts are affected by the fork point).
    AccountsAtEpoch = 24,

    /// **`StorageAtEpoch`** —
    /// `epoch (u64 BE) → Vec<(space, address, key)> (RLP)`.
    ///
    /// Storage-slot equivalent of [`AccountsAtEpoch`].
    StorageAtEpoch = 25,

    /// **`CodeHistory`** —
    /// `code_hash (32B) → RoaringBitmap<epoch>`.
    ///
    /// For every deployed contract-code blob, the set of epochs where
    /// a new account started using it. Rare-writes; useful to prune
    /// [`CodeStore`] entries whose bitmap is empty (nobody references
    /// them anymore).
    CodeHistory = 26,

    // ------------------------------------------------------------------
    // 4. Consensus state
    // ------------------------------------------------------------------

    /// **`ChainMetadata`** — `single_key_id (u8) → ChainMeta (RLP)`.
    ///
    /// Small MISC-like table for single-key items:
    ///   - best_epoch, best_hash
    ///   - confirmed_epoch, confirmed_hash
    ///   - executor watermark
    ///   - sync/consensus statistics counters
    ///
    /// Each item indexed by a stable `u8` id so we don't need one
    /// column per counter. Currently corresponds to what the ParityDB
    /// layout called `COL_MISC`.
    ChainMetadata = 27,

    /// **`SnapshotInfo`** —
    /// `snapshot_epoch_id (32B) → SnapshotInfo (RLP)`.
    ///
    /// Metadata for each stored snapshot: era boundary, associated
    /// state root, expected size on disk, availability flag.
    SnapshotInfo = 28,

    /// **`StateAvailabilityBoundary`** —
    /// `single_key (u8) → { lower_bound (u64 BE), upper_bound (u64 BE),
    ///                       main_chain (Vec<block_hash>) }`.
    ///
    /// The state-availability window: which epochs the node has state
    /// for. The `main_chain` field cover-pins the deferred-execution
    /// window so pivot changes can be detected without walking every
    /// header.
    StateAvailabilityBoundary = 29,

    /// **`ForceConfirmed`** —
    /// `timer_height (u64 BE) → block_hash (32B)`.
    ///
    /// The force-confirm decision at each `TIMER_CHAIN_BEACON_INTERVAL`
    /// tick. Written once per interval; never rewritten. Consumers
    /// use this to enforce finality: reorgs past a force-confirmed
    /// hash are rejected outright.
    ForceConfirmed = 30,

    /// **`RandomxSeedByEpoch`** —
    /// `seed_segment_start_epoch (u64 BE) → seed_hash (32B)`.
    ///
    /// The seed used to validate PoW for blocks in the segment
    /// `[start, start + RANDOMX_EPOCH_LENGTH)`. Since DD-1 landed,
    /// segment length == era length (20 000 epochs). Non-archive
    /// nodes need at least the last two entries here (per §4a.6) so
    /// they can validate their own catchup across a rotation.
    RandomxSeedByEpoch = 31,

    // ------------------------------------------------------------------
    // 5. Shielded pool (native today; will migrate to Space::Shielded)
    // ------------------------------------------------------------------

    /// **`ShieldedFrontier`** — `level (u32 BE) → hash (32B)`.
    ///
    /// The 32-level Merkle-tree frontier used for note-commitment
    /// insertion. Written on every shielded-in operation.
    ShieldedFrontier = 32,

    /// **`ShieldedRoots`** — `slot (u32 BE) → root_hash (32B)`.
    ///
    /// 64-slot rotating history of shielded-tree roots. Lets a
    /// shielded-out proof reference a root up to 64 tree updates old
    /// without requiring the exact latest root — small anonymity-set
    /// benefit + async proof generation friendly.
    ShieldedRoots = 33,

    /// **`ShieldedNullifiers`** — `nullifier_hash (32B) → () (empty)`.
    ///
    /// Set-typed table: a nullifier being present means the note has
    /// been spent. Prevents double-spend. Grows monotonically forever
    /// (fundamental to shielded-tx semantics).
    ShieldedNullifiers = 34,

    /// **`ShieldedIndices`** — `single_key (u8) → indices (RLP)`.
    ///
    /// Small counter table for the shielded pool: latest `leaf_index`,
    /// latest `root_index`, VK hash, VK length. Corresponds to the
    /// current `shielded:leaf_index` / `shielded:root_index` /
    /// `shielded:vk_hash` / `shielded:vk_len` keys.
    ShieldedIndices = 35,

    /// **`ShieldedVKWords`** — `word_index (u32 BE) → 32-byte word`.
    ///
    /// The Groth16 verifying key, chunked into 32-byte words for
    /// storage. Streamed on first shielded-tx execution to hydrate the
    /// prepared-VK cache in memory (`PowComputer`-analog for ZK
    /// verification).
    ShieldedVKWords = 36,

    // ------------------------------------------------------------------
    // 6. Voluntary data availability (§10 Q4)
    // ------------------------------------------------------------------

    /// **`EraAttestations`** —
    /// `(era_index (u64 BE), operator_pubkey (33B)) → signature (65B)`.
    ///
    /// Off-consensus attestations from archive operators claiming they
    /// hold the era-archive artifact for a given `era_index`. Gossiped
    /// via mempool; consumed by nodes with a `trusted_operators`
    /// client-side policy configured. Chain progression is never
    /// gated by contents of this table (per §10 Q4).
    EraAttestations = 37,

    /// **`EraArtifactRoots`** — `era_index (u64 BE) → root_hash (32B)`.
    ///
    /// The deterministic content-hash of the era-archive artifact
    /// (the tar.zst that operators sign attestations for). Computed
    /// by the artifact producer at era finalization; used by
    /// downloaders to verify the artifact matches what attestations
    /// point to.
    EraArtifactRoots = 38,

    // ------------------------------------------------------------------
    // 7. Static-file coordination (Phase 4)
    // ------------------------------------------------------------------

    /// **`StaticFileIndex`** —
    /// `column_type (u8) → Vec<SegmentDescriptor (RLP)>`.
    ///
    /// Records which block-range segments have been offloaded from
    /// MDBX to static files under `bodies/N-M.seg` /
    /// `receipts/N-M.seg` / `traces/N-M.seg`. Reads that miss in MDBX
    /// consult this table to decide which static file to open —
    /// avoids probing every segment file on every miss.
    ///
    /// One entry per column-type (bodies / receipts / traces): the
    /// descriptor lists every segment, its block range, its file
    /// path, and its content hash for verification.
    StaticFileIndex = 39,

    /// **`PrunedRanges`** —
    /// `(column_type (u8), block_range_start (u64 BE)) →
    ///  { end (u64 BE), pruned_at_epoch (u64 BE), reason (u8) }`.
    ///
    /// Records which block ranges have been removed from MDBX after
    /// migration to static files (or after retention-window pruning
    /// on non-archive nodes). Consulted before returning "not found"
    /// to a query — lets us respond with a specific "pruned since
    /// epoch X" error instead of an ambiguous absence.
    PrunedRanges = 40,

    // ------------------------------------------------------------------
    // 8. Migration / schema (Phase 7)
    // ------------------------------------------------------------------

    /// **`SchemaVersion`** —
    /// `single_key (u8) → { version (u32), applied_at_epoch (u64 BE) }`.
    ///
    /// Records the storage schema version the DB was last written by.
    /// Consulted at startup: a mismatch triggers migration (if the
    /// downgrade path exists) or a refusal to open (if not). The
    /// version bumps monotonically; the phase-7 migration tool
    /// records the from → to bump here atomically with the actual
    /// migration commit.
    SchemaVersion = 41,

    /// **`MigrationCheckpoint`** —
    /// `migration_id (u32 BE) →
    ///  { last_committed_key (Vec<u8>), progress_epoch (u64 BE),
    ///    started_at (u64 BE), stage (u8) }`.
    ///
    /// Resumable migration cursor: the ParityDB → MDBX migration
    /// tool writes here every N thousand keys so a crash mid-migration
    /// resumes from the last committed point rather than from
    /// scratch. Cleared once migration completes and the source is
    /// dropped.
    MigrationCheckpoint = 42,

    /// **`IntegrityChecksums`** —
    /// `epoch (u64 BE) → { table_bitmap (RoaringBitmap),
    ///                     sha3_of_sorted_kv (32B) }`.
    ///
    /// Periodic snapshot checksums used by the corruption-recovery
    /// mode (Phase 7). At each era boundary the storage layer hashes
    /// the sorted (key, value) sequence of every listed table into
    /// a single 32-byte digest and stores it here. On suspected
    /// corruption, we recompute the digest and compare — a mismatch
    /// pinpoints which table diverged.
    IntegrityChecksums = 43,

    // ------------------------------------------------------------------
    // 9. RPC-derived indices (log-query acceleration)
    // ------------------------------------------------------------------

    /// **`EpochLogsBloom`** — `epoch (u64 BE) → Bloom (256B)`.
    ///
    /// Per-epoch bloom filter merged across every block in the epoch.
    /// Consulted by `eth_getLogs` / `mazze_getLogs` at query time —
    /// if the requested topics/addresses aren't in the epoch's bloom,
    /// the entire epoch can be skipped without scanning any block
    /// receipt. This is the single biggest RPC-latency lever after
    /// the flat state itself.
    EpochLogsBloom = 44,

    /// **`AddressTouchedBitmap`** —
    /// `(space (1B), address (20B)) → RoaringBitmap<epoch>`.
    ///
    /// For every address (regardless of native/eSpace), the set of
    /// epochs where it appeared as a `log.address`. Cheap intersect
    /// with a query's `[address in ...]` filter. Complement to
    /// [`AccountHistory`] — that tracks state changes; this tracks
    /// log emissions from the address.
    AddressTouchedBitmap = 45,

    /// **`TopicTouchedBitmap`** —
    /// `(topic_index (u8), topic (32B)) → RoaringBitmap<epoch>`.
    ///
    /// One entry per (topic slot, topic value) pair, e.g. an ERC-20
    /// Transfer event has `topic[0] = sha3("Transfer(address,address,
    /// uint256)")` and can be intersected with an address filter on
    /// `topic[1]`. Roaring compression makes this small; per-index
    /// separation lets the filter engine avoid cross-slot false
    /// positives.
    TopicTouchedBitmap = 46,

    // ------------------------------------------------------------------
    // 10. Executor pins / reward windows (Phase 2)
    // ------------------------------------------------------------------

    /// **`RewardWindowMetadata`** —
    /// `epoch (u64 BE) → { reward_window_hashes (Vec<block_hash>),
    ///                     pinned_from_epoch (u64 BE) }`.
    ///
    /// The executor's reward window: for each executed epoch, the
    /// block hashes whose `Arc<Block>` must stay resident so reward
    /// calculation can walk the referee subtree. Made explicit here
    /// so the executor can compute its `pending_execution_count`
    /// (§Q7a) precisely instead of via the current heuristic.
    ///
    /// Cleaned up as its window ages out of the deferred-execution
    /// horizon.
    RewardWindowMetadata = 47,

    // ------------------------------------------------------------------
    // 11. Genesis / boot verification
    // ------------------------------------------------------------------

    /// **`GenesisMetadata`** — `single_key (u8) → GenesisSummary (RLP)`.
    ///
    /// Records the genesis parameters the DB was booted with: genesis
    /// hash, chain id, initial difficulty, timestamps, treasury root.
    /// Enables reproducible-genesis verification: on relaunch the
    /// consensus code compares its computed genesis against the
    /// stored one and refuses to open the DB if they diverge (early
    /// signal of a config drift, not a silent fork).
    GenesisMetadata = 48,
}

/// Total number of MDBX columns. Update on adding a variant.
///
/// The `KvdbMdbx` env is created with `set_max_tables(64)` (see
/// [`DEFAULT_MAX_TABLES`](super::kvdb_mdbx)); we have headroom for
/// ~15 more logical tables before that cap becomes a concern.
pub const NUM_COLUMNS: u32 = 49;

impl Column {
    /// The stable `u32` id assigned to this column. `#[repr(u32)]` on
    /// the enum makes `variant as u32 == variant.id()` at compile time.
    #[inline]
    pub const fn id(self) -> u32 {
        self as u32
    }

    /// Full enumeration in declaration order. Useful for schema
    /// verification (open every column at startup, sanity-check ids)
    /// and for operator-dashboard iteration (poll `stats()` per column).
    pub const ALL: &'static [Column] = &[
        Column::BlockHeaders,
        Column::BlockBodies,
        Column::BlockReceipts,
        Column::BlockTraces,
        Column::BlockExecutionCommitment,
        Column::BlockRewards,
        Column::TxIndex,
        Column::HashByNumber,
        Column::NumberByHash,
        Column::EpochBlocks,
        Column::BestPivot,
        Column::TimerChain,
        Column::BlockIsTimer,
        Column::ChildrenByParent,
        Column::BlamedHeaderVerifiedRoots,
        Column::VerifiedInvalidBlocks,
        Column::PlainAccount,
        Column::PlainStorage,
        Column::CodeStore,
        Column::StorageRoots,
        Column::AccountHistory,
        Column::StorageHistory,
        Column::AccountChangeSet,
        Column::StorageChangeSet,
        Column::AccountsAtEpoch,
        Column::StorageAtEpoch,
        Column::CodeHistory,
        Column::ChainMetadata,
        Column::SnapshotInfo,
        Column::StateAvailabilityBoundary,
        Column::ForceConfirmed,
        Column::RandomxSeedByEpoch,
        Column::ShieldedFrontier,
        Column::ShieldedRoots,
        Column::ShieldedNullifiers,
        Column::ShieldedIndices,
        Column::ShieldedVKWords,
        Column::EraAttestations,
        Column::EraArtifactRoots,
        Column::StaticFileIndex,
        Column::PrunedRanges,
        Column::SchemaVersion,
        Column::MigrationCheckpoint,
        Column::IntegrityChecksums,
        Column::EpochLogsBloom,
        Column::AddressTouchedBitmap,
        Column::TopicTouchedBitmap,
        Column::RewardWindowMetadata,
        Column::GenesisMetadata,
    ];

    /// Human-readable name — same as the variant identifier, hoisted
    /// out so logs and metrics can format columns without recompiling.
    pub const fn name(self) -> &'static str {
        match self {
            Column::BlockHeaders => "BlockHeaders",
            Column::BlockBodies => "BlockBodies",
            Column::BlockReceipts => "BlockReceipts",
            Column::BlockTraces => "BlockTraces",
            Column::BlockExecutionCommitment => "BlockExecutionCommitment",
            Column::BlockRewards => "BlockRewards",
            Column::TxIndex => "TxIndex",
            Column::HashByNumber => "HashByNumber",
            Column::NumberByHash => "NumberByHash",
            Column::EpochBlocks => "EpochBlocks",
            Column::BestPivot => "BestPivot",
            Column::TimerChain => "TimerChain",
            Column::BlockIsTimer => "BlockIsTimer",
            Column::ChildrenByParent => "ChildrenByParent",
            Column::BlamedHeaderVerifiedRoots => "BlamedHeaderVerifiedRoots",
            Column::VerifiedInvalidBlocks => "VerifiedInvalidBlocks",
            Column::PlainAccount => "PlainAccount",
            Column::PlainStorage => "PlainStorage",
            Column::CodeStore => "CodeStore",
            Column::StorageRoots => "StorageRoots",
            Column::AccountHistory => "AccountHistory",
            Column::StorageHistory => "StorageHistory",
            Column::AccountChangeSet => "AccountChangeSet",
            Column::StorageChangeSet => "StorageChangeSet",
            Column::AccountsAtEpoch => "AccountsAtEpoch",
            Column::StorageAtEpoch => "StorageAtEpoch",
            Column::CodeHistory => "CodeHistory",
            Column::ChainMetadata => "ChainMetadata",
            Column::SnapshotInfo => "SnapshotInfo",
            Column::StateAvailabilityBoundary => "StateAvailabilityBoundary",
            Column::ForceConfirmed => "ForceConfirmed",
            Column::RandomxSeedByEpoch => "RandomxSeedByEpoch",
            Column::ShieldedFrontier => "ShieldedFrontier",
            Column::ShieldedRoots => "ShieldedRoots",
            Column::ShieldedNullifiers => "ShieldedNullifiers",
            Column::ShieldedIndices => "ShieldedIndices",
            Column::ShieldedVKWords => "ShieldedVKWords",
            Column::EraAttestations => "EraAttestations",
            Column::EraArtifactRoots => "EraArtifactRoots",
            Column::StaticFileIndex => "StaticFileIndex",
            Column::PrunedRanges => "PrunedRanges",
            Column::SchemaVersion => "SchemaVersion",
            Column::MigrationCheckpoint => "MigrationCheckpoint",
            Column::IntegrityChecksums => "IntegrityChecksums",
            Column::EpochLogsBloom => "EpochLogsBloom",
            Column::AddressTouchedBitmap => "AddressTouchedBitmap",
            Column::TopicTouchedBitmap => "TopicTouchedBitmap",
            Column::RewardWindowMetadata => "RewardWindowMetadata",
            Column::GenesisMetadata => "GenesisMetadata",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Column::id()` returns the discriminant. `Column::ALL` matches
    /// the discriminants in ascending order and has no gaps —
    /// invariants that guarantee a stable on-disk layout and let the
    /// schema-verification code at Phase 1 step 7 iterate over every
    /// column deterministically.
    #[test]
    fn ids_stable_and_dense() {
        // Discriminants are exactly 0..NUM_COLUMNS in order.
        assert_eq!(Column::ALL.len() as u32, NUM_COLUMNS);
        for (i, col) in Column::ALL.iter().enumerate() {
            assert_eq!(
                col.id(),
                i as u32,
                "column {:?} at ALL[{}] should have id {}, has {}",
                col,
                i,
                i,
                col.id()
            );
        }

        // Cross-check: NUM_COLUMNS is the max id + 1, no gaps.
        let max_id =
            Column::ALL.iter().map(|c| c.id()).max().unwrap();
        assert_eq!(max_id + 1, NUM_COLUMNS);
    }

    /// Every column has a distinct name. Names are the only free-form
    /// text in the catalog — this test catches copy-paste mistakes in
    /// the `Column::name()` match arm.
    #[test]
    fn names_are_unique() {
        use std::collections::HashSet;
        let names: HashSet<&'static str> =
            Column::ALL.iter().map(|c| c.name()).collect();
        assert_eq!(names.len(), Column::ALL.len());
    }

    /// The declared name matches the variant identifier for every
    /// column. This is the guard against a rename-in-source-drift
    /// bug where the enum variant is renamed but `name()` still
    /// returns the old string (or vice versa).
    #[test]
    fn name_matches_debug_repr() {
        for col in Column::ALL {
            let dbg = format!("{:?}", col);
            assert_eq!(
                dbg,
                col.name(),
                "Debug repr {:?} disagrees with name() {:?}",
                dbg,
                col.name()
            );
        }
    }
}
