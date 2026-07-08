// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

/// The in-mem snapshot_info map and the on-disk MDBX column
/// `SnapshotInfo` are always in sync.
///
/// Phase 5b: swapped from `KvdbParitydb` on its own paritydb env
/// to `KvdbMdbx` on the shared hot-tier MDBX env
/// ([`MdbxColumn::SnapshotInfo`]). No more per-subsystem paritydb
/// envs; the whole storage layer talks to one env after 5b.
pub struct PersistedSnapshotInfoMap {
    // Db to persist snapshot_info.
    snapshot_info_db: crate::impls::storage_db::kvdb_mdbx::KvdbMdbx,
    // In memory snapshot_info_map_by_epoch.
    snapshot_info_map_by_epoch: HashMap<EpochId, SnapshotInfo>,
}

impl PersistedSnapshotInfoMap {
    fn new(
        snapshot_info_db: crate::impls::storage_db::kvdb_mdbx::KvdbMdbx,
    ) -> Result<Self> {
        let mut result = Self {
            snapshot_info_map_by_epoch: Default::default(),
            snapshot_info_db,
        };
        result.load_persist_state()?;
        Ok(result)
    }

    fn insert(
        &mut self, epoch: &EpochId, snapshot_info: SnapshotInfo,
    ) -> Result<()> {
        let rlp_bytes = snapshot_info.rlp_bytes();
        self.snapshot_info_map_by_epoch
            .insert(epoch.clone(), snapshot_info);
        // KvdbMdbx implements KeyValueDbTrait; `put` is the same
        // signature the paritydb variant exposed.
        use crate::storage_db::key_value_db::KeyValueDbTrait;
        self.snapshot_info_db.put(epoch.as_ref(), &rlp_bytes)?;
        Ok(())
    }

    fn get_map(&self) -> &HashMap<EpochId, SnapshotInfo> {
        &self.snapshot_info_map_by_epoch
    }

    fn get(&self, epoch: &EpochId) -> Option<&SnapshotInfo> {
        self.snapshot_info_map_by_epoch.get(epoch)
    }

    fn remove(&mut self, epoch: &EpochId) -> Result<()> {
        self.snapshot_info_map_by_epoch.remove(epoch);
        use crate::storage_db::key_value_db::KeyValueDbTrait;
        self.snapshot_info_db.delete(epoch.as_ref())?;
        Ok(())
    }

    // Unsafe because the in mem map isn't in sync with the db.
    unsafe fn remove_in_mem_only(
        &mut self, epoch: &EpochId,
    ) -> Option<SnapshotInfo> {
        self.snapshot_info_map_by_epoch.remove(epoch)
    }

    fn load_persist_state(&mut self) -> Result<()> {
        // MDBX iteration: read the whole column into a Vec via
        // `iter_range_owned` and rebuild the in-mem map. The
        // per-epoch entries fit trivially in memory (dozens to
        // low thousands even on archive nodes).
        for (key, value) in self
            .snapshot_info_db
            .iter_range_owned(b"", None)?
            .into_iter()
        {
            if key.len() != EpochId::len_bytes() {
                return Err(DecoderError::RlpInvalidLength.into());
            }
            self.snapshot_info_map_by_epoch.insert(
                EpochId::from_slice(&key),
                SnapshotInfo::decode(&Rlp::new(&value))?,
            );
        }
        Ok(())
    }
}

// FIXME: correctly order code blocks.
pub struct StorageManager {
    delta_db_manager: Arc<DeltaDbManager>,
    delta_mpt_open_db_lru: Arc<OpenDeltaDbLru<DeltaDbManager>>,
    snapshot_manager: Box<
        dyn SnapshotManagerTrait<
                SnapshotDb = SnapshotDb,
                SnapshotDbManager = SnapshotDbManager,
            > + Send
            + Sync,
    >,
    delta_mpts_id_gen: Mutex<DeltaMptIdGen>,
    delta_mpts_node_memory_manager: Arc<DeltaMptsNodeMemoryManager>,

    maybe_db_errors: MaybeDeltaTrieDestroyErrors,
    snapshot_associated_mpts_by_epoch: RwLock<
        HashMap<EpochId, (Option<Arc<DeltaMpt>>, Option<Arc<DeltaMpt>>)>,
    >,

    // Lock order: while this is locked, in
    // check_make_register_snapshot_background, snapshot_info_map_by_epoch
    // is locked later.
    pub in_progress_snapshotting_tasks:
        RwLock<HashMap<EpochId, Arc<RwLock<InProgressSnapshotTask>>>>,
    in_progress_snapshot_finish_signaler: Arc<Mutex<Sender<Option<EpochId>>>>,
    in_progress_snapshotting_joiner: Mutex<Option<JoinHandle<()>>>,

    // The order doesn't matter as long as parent snapshot comes before
    // children snapshots.
    // Note that for archive node the list here is just a subset of what's
    // available.
    //
    // Lock order: while this is locked, in load_persist_state and
    // state_manager.rs:get_state_trees_for_next_epoch
    // snapshot_associated_mpts_by_epoch is locked later.
    current_snapshots: RwLock<Vec<SnapshotInfo>>,
    // Lock order: while this is locked, in register_new_snapshot and
    // load_persist_state, current_snapshots and
    // snapshot_associated_mpts_by_epoch are locked later.
    pub snapshot_info_map_by_epoch: RwLock<PersistedSnapshotInfoMap>,

    last_confirmed_snapshottable_epoch_id: Mutex<Option<EpochId>>,

    pub storage_conf: StorageConfiguration,

    // used during startup for the next compute epoch
    pub intermediate_trie_root_merkle: RwLock<Option<MerkleHash>>,

    pub persist_state_from_initialization:
        RwLock<Option<(Option<EpochId>, HashSet<EpochId>, u64, Option<u64>)>>,

    /// Hot-tier MDBX environment, opened at startup when
    /// `storage_conf.state_db_backend == StateDbBackend::Mdbx(_)`.
    /// `None` for the `ParityDb` fallback. Consumers (executor's live
    /// state cache, revm's `MazzeDatabase`) reach it via
    /// [`StorageManager::mdbx_env`].
    ///
    /// **Wiring status**: this field is *opened* by Phase B+C of the
    /// storage cleanup plan but not yet *consumed* — see
    /// docs/storage-architecture.md for the next integration step.
    mdbx_env: Option<Arc<crate::impls::storage_db::kvdb_mdbx::MdbxEnv>>,
    /// Dedicated MDBX env for the Phase 5c snapshot tier at
    /// `storage_db/mdbx_snapshot/`. NOT shared with `mdbx_env`
    /// because snapshot data is cold, bulky, and has its own
    /// writer-lock domain (see design doc §2.3.0). Opened
    /// **growth-enabled** so the map can grow up to
    /// `SnapshotMdbxConfig::max_mb` — the capacity gauge fires a
    /// warn well before it hits.
    ///
    /// **Wiring status (pre-work #3)**: opened, exposed via
    /// [`Self::snapshot_mdbx_env`], gauge-monitored via
    /// [`Self::sample_snapshot_mdbx_capacity`]. Consumed by
    /// `SnapshotDbManagerMdbx` in the 5c main commit.
    snapshot_mdbx_env:
        Option<Arc<crate::impls::storage_db::kvdb_mdbx::MdbxEnv>>,
    /// Snapshot env's configured `max_bytes` cached alongside the
    /// env for the capacity-percentage gauge — avoids re-multiplying
    /// on every sample and keeps the alert threshold checkable in
    /// one place.
    snapshot_mdbx_max_bytes: u64,
}

impl MallocSizeOf for StorageManager {
    fn size_of(&self, ops: &mut MallocSizeOfOps) -> usize {
        // TODO: Snapshot DB memory usage is not accounted for here.
        let mut size = 0;
        size += self.delta_mpts_node_memory_manager.size_of(ops);
        size += self.snapshot_associated_mpts_by_epoch.size_of(ops);
        size
    }
}

/// Struct which makes sure that the delta mpt is properly ref-counted and
/// released.
pub struct DeltaDbReleaser {
    pub storage_manager: Weak<StorageManager>,
    pub snapshot_epoch_id: EpochId,
    pub mpt_id: DeltaMptId,
}

impl Drop for DeltaDbReleaser {
    fn drop(&mut self) {
        // Don't drop any delta mpt at graceful shutdown because those remaining
        // DeltaMPTs are useful.

        // Note that when an error happens in db, the program should fail
        // gracefully, but not in destructor.
        Weak::upgrade(&self.storage_manager).map(|storage_manager| {
            storage_manager.release_delta_mpt_actions_in_drop(
                &self.snapshot_epoch_id,
                self.mpt_id,
            )
        });
    }
}

/// State of an in-flight snapshot-creation background thread.
///
/// Cancellation: `cancel_requested` is set by
/// [`StorageManager::maintain_snapshots_main_chain_confirmed`] when
/// the snapshot's target is determined to be on a non-canonical fork.
/// The background thread checks the flag at merge-step boundaries
/// (see [`SnapshotDbManagerParityDb::new_snapshot_by_merging`]) and,
/// on `true`, aborts early, deletes the temp directory, and returns
/// `Err(ErrorKind::SnapshotCowCancelled)` so the joiner knows the
/// result is a cancellation rather than a genuine failure.
///
/// See `docs/checkpoint-snapshot-lifecycle.md` §5.2 and Phase B.1 of
/// the lifecycle plan.
pub struct InProgressSnapshotTask {
    snapshot_info: SnapshotInfo,
    thread_handle: Option<thread::JoinHandle<Result<()>>>,
    /// Set to `true` to request that the background thread abort at
    /// the next merge-step boundary. Wrapped in `Arc` so the thread
    /// closure can observe the same flag without re-locking the
    /// outer `RwLock<InProgressSnapshotTask>`.
    cancel_requested: Arc<AtomicBool>,
}

impl InProgressSnapshotTask {
    // Returns None if the thread has been joined already. Returns the
    // background snapshotting result when the thread is first joined.
    pub fn join(&mut self) -> Option<Result<()>> {
        if let Some(join_handle) = self.thread_handle.take() {
            match join_handle.join() {
                Ok(task_result) => Some(task_result),
                Err(_) => Some(Err(ErrorKind::ThreadPanicked(format!(
                    "Background Snapshotting for {:?} panicked.",
                    self.snapshot_info
                ))
                .into())),
            }
        } else {
            None
        }
    }

    /// Request that this in-flight snapshot be cancelled at the next
    /// merge-step boundary. Non-blocking; safe to call multiple times.
    /// The background thread is responsible for releasing temp-dir
    /// resources and returning `Err(SnapshotCowCancelled)`. See
    /// `docs/checkpoint-snapshot-lifecycle.md` §5.2.
    pub fn request_cancel(&self) {
        self.cancel_requested
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Shared handle to the cancel flag. The background thread holds
    /// one of these and polls it at merge-step boundaries.
    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel_requested)
    }
}

impl StorageManager {
    pub fn new_arc(
        /* TODO: Add node type, full node or archive node */
        storage_conf: StorageConfiguration,
    ) -> Result<Arc<Self>> {
        let storage_dir = storage_conf.path_storage_dir.as_path();
        debug!(
            "new StorageManager within storage_dir {}",
            storage_dir.display()
        );
        if !storage_dir.exists() {
            fs::create_dir_all(storage_dir)?;
        }

        // G-NET-2 — Surface the retention-knob value loud at startup
        // because its safe value is a NETWORK-AGGREGATE concern, not a
        // per-node one. If every operator flips it to `false`, fresh
        // joiners fall off into genesis replay even though no
        // individual node looks wrong. See docs/flow-audit.md §5.8.
        if storage_conf.keep_snapshot_before_stable_checkpoint {
            info!(
                "storage: keep_snapshot_before_stable_checkpoint=true (default). \
                 This node retains the pre-stable snapshot generation so a fresh \
                 joiner can still bootstrap via this peer right after an era \
                 rollover. See docs/checkpoint-snapshot-lifecycle.md §6."
            );
        } else {
            warn!(
                "storage: keep_snapshot_before_stable_checkpoint=FALSE. \
                 If a fresh joiner connects right after an era rollover and \
                 every connected peer of theirs also runs with this setting, \
                 they will fall back to multi-day genesis replay even though \
                 the network has consensus. Recommended only for explicitly \
                 disk-constrained archive variants. See docs/flow-audit.md §5.8 \
                 + docs/checkpoint-snapshot-lifecycle.md §6."
            );
        }

        // Phase 5e: `StateDbBackend` is now `Mdbx`-only — the
        // ParityDB fallback went with the paritydb impl files.
        // The env is still `Option<Arc<MdbxEnv>>` for API-shape
        // parity with getters/wiring downstream; it is always
        // `Some` under the current single-variant enum.
        let mdbx_env = {
            let crate::StateDbBackend::Mdbx(cfg) =
                &storage_conf.state_db_backend;
            let mdbx_dir = storage_dir.join("mdbx");
            let map_size_bytes = cfg
                .map_size_mb
                .unwrap_or(
                    crate::impls::defaults::DEFAULT_MDBX_MAP_SIZE_MB,
                )
                .saturating_mul(1024 * 1024)
                as usize;
            let sync_mode = cfg.sync_mode.to_libmdbx();
            let env =
                crate::impls::storage_db::kvdb_mdbx::MdbxEnv::open_with_map_size_and_sync(
                    &mdbx_dir,
                    map_size_bytes,
                    sync_mode,
                )?;
            info!(
                "Opened MDBX hot tier at {} (map_size={} MB, sync_mode={:?})",
                mdbx_dir.display(),
                map_size_bytes / (1024 * 1024),
                cfg.sync_mode
            );
            Some(env)
        };

        // Phase 5c pre-work #3: open the dedicated snapshot MDBX env
        // with a growth-enabled geometry. This env is separate from
        // the hot `mdbx_env` — snapshot data is cold, bulky, and has
        // its own writer-lock domain per design doc §2.3.0. The
        // physical dir is created lazily by `open_with_geometry` so
        // fresh nodes don't need extra setup. `SnapshotDbManagerMdbx`
        // will consume the env in the 5c main commit; pre-work #3
        // just opens it and stands up the capacity gauge.
        let snapshot_cfg = &storage_conf.snapshot_mdbx_config;
        let snapshot_initial_bytes =
            (snapshot_cfg.initial_mb as usize).saturating_mul(1024 * 1024);
        let snapshot_max_bytes =
            (snapshot_cfg.max_mb as usize).saturating_mul(1024 * 1024);
        let snapshot_growth_bytes = (snapshot_cfg.growth_step_mb as usize)
            .saturating_mul(1024 * 1024);
        let snapshot_mdbx_env = {
            // Reuse the hot-env sync mode for the snapshot env —
            // operators pick one durability tier per node profile;
            // the snapshot env's writes are only during era merges
            // + destroy so a slightly relaxed sync is safe (a
            // partial snapshot on crash is reconstructed by
            // `scan_persist_state`'s marker-driven recovery).
            let crate::StateDbBackend::Mdbx(hot_cfg) =
                &storage_conf.state_db_backend;
            let sync_mode = hot_cfg.sync_mode.to_libmdbx();
            let env = crate::impls::storage_db::kvdb_mdbx::MdbxEnv
                ::open_with_geometry_and_sync(
                    &storage_conf.path_snapshot_mdbx_dir,
                    snapshot_initial_bytes,
                    snapshot_max_bytes,
                    snapshot_growth_bytes,
                    sync_mode,
                )?;
            info!(
                "Opened MDBX snapshot tier at {} (initial={} MB, \
                 max={} MB, growth_step={} MB, sync_mode={:?})",
                storage_conf.path_snapshot_mdbx_dir.display(),
                snapshot_cfg.initial_mb,
                snapshot_cfg.max_mb,
                snapshot_cfg.growth_step_mb,
                hot_cfg.sync_mode
            );
            Some(env)
        };
        let snapshot_mdbx_max_bytes = snapshot_max_bytes as u64;

        // Snapshot info now lives in `MdbxColumn::SnapshotInfo` on
        // the shared MDBX env — no per-subsystem paritydb any more.
        let snapshot_info_kvdb = {
            use crate::impls::storage_db::{
                kvdb_mdbx::KvdbMdbx, mdbx_columns::Column,
            };
            KvdbMdbx::with_column(
                Arc::clone(
                    mdbx_env
                        .as_ref()
                        .expect("MDBX env just opened above"),
                ),
                Column::SnapshotInfo.id(),
            )
        };

        let snapshot_info_map =
            PersistedSnapshotInfoMap::new(snapshot_info_kvdb)?;

        let (
            in_progress_snapshot_finish_signaler,
            in_progress_snapshot_finish_signal_receiver,
        ) = channel();

        // Phase 4c cutover: DeltaDbManager is now the MDBX-native
        // impl (see `impls/state_manager.rs`). It requires the
        // shared MDBX env; fail loudly if the operator has us in
        // paritydb-only mode.
        let delta_env = mdbx_env
            .as_ref()
            .cloned()
            .ok_or_else(|| ErrorKind::Msg(
                "DeltaDbManagerMdbx requires the shared MDBX env; \
                 set `state_db_type = \"mdbx\"` in hydra.toml to \
                 enable it. See docs/internal/\
                 storage-delta-mpt-migration.md §2.4.".to_string()
            ))?;
        let delta_db_manager = Arc::new(DeltaDbManager::new(
            delta_env,
            storage_conf.path_delta_mpts_dir.clone(),
        )?);

        let new_storage_manager_result = Ok(Arc::new(Self {
            delta_db_manager: delta_db_manager.clone(),
            delta_mpt_open_db_lru: Arc::new(OpenDeltaDbLru::new(
                delta_db_manager.clone(),
                storage_conf.max_open_mpt_count,
            )?),
            snapshot_manager: Box::new(SnapshotManager::<SnapshotDbManager> {
                // Phase 5c wiring: SnapshotDbManagerMdbx takes the
                // dedicated snapshot env opened above +
                // `snapshot_path` (kept for API parity) +
                // max_open_snapshots. Phase 5d deleted the
                // `use_isolated_db_for_mpt_table*` knobs the
                // paritydb constructor accepted — the mode was
                // already non-functional (design doc §3 / R1.3).
                snapshot_db_manager: SnapshotDbManager::new(
                    Arc::clone(
                        snapshot_mdbx_env.as_ref().expect(
                            "snapshot MDBX env just opened above",
                        ),
                    ),
                    storage_conf.path_snapshot_dir.clone(),
                    storage_conf.max_open_snapshots,
                )?,
            }),
            delta_mpts_id_gen: Default::default(),
            delta_mpts_node_memory_manager: Arc::new(
                DeltaMptsNodeMemoryManager::new(
                    storage_conf.delta_mpts_cache_start_size,
                    storage_conf.delta_mpts_cache_size,
                    storage_conf.delta_mpts_slab_idle_size,
                    storage_conf.delta_mpts_node_map_vec_size,
                    DeltaMptsCacheAlgorithm::new(
                        storage_conf.delta_mpts_cache_size,
                    ),
                ),
            ),
            maybe_db_errors: MaybeDeltaTrieDestroyErrors::new(),
            snapshot_associated_mpts_by_epoch: Default::default(),
            in_progress_snapshotting_tasks: Default::default(),
            in_progress_snapshot_finish_signaler: Arc::new(Mutex::new(
                in_progress_snapshot_finish_signaler,
            )),
            in_progress_snapshotting_joiner: Default::default(),
            current_snapshots: Default::default(),
            snapshot_info_map_by_epoch: RwLock::new(snapshot_info_map),
            last_confirmed_snapshottable_epoch_id: Default::default(),
            storage_conf,
            intermediate_trie_root_merkle: RwLock::new(None),
            persist_state_from_initialization: RwLock::new(None),
            mdbx_env,
            snapshot_mdbx_env,
            snapshot_mdbx_max_bytes,
        }));

        let storage_manager_arc =
            new_storage_manager_result.as_ref().unwrap().clone();
        *new_storage_manager_result.as_ref().unwrap().in_progress_snapshotting_joiner.lock() =
            Some(thread::Builder::new()
                .name("Background Snapshot Joiner".to_string()).spawn(
            move || {
                for exit_program_or_finished_snapshot in
                    in_progress_snapshot_finish_signal_receiver.iter() {
                    if exit_program_or_finished_snapshot.is_none() {
                        break;
                    }
                    let finished_snapshot = exit_program_or_finished_snapshot.unwrap();
                    if let Some(task) = storage_manager_arc
                        .in_progress_snapshotting_tasks.read().get(&finished_snapshot) {
                        let snapshot_result = task.write().join();
                        if let Some(Err(e)) = snapshot_result {
                            warn!(
                                "Background snapshotting for {:?} failed with {}",
                                finished_snapshot, e);
                        }
                    }
                    storage_manager_arc.in_progress_snapshotting_tasks
                        .write().remove(&finished_snapshot);
                }
                // TODO: handle program exit signal.
            }
        )?);

        new_storage_manager_result
            .as_ref()
            .unwrap()
            .load_persist_state()?;

        new_storage_manager_result
    }

    pub fn persisted_max_epoch_height(&self) -> u64 {
        self.persist_state_from_initialization
            .read()
            .as_ref()
            .map(|(_, _, max_epoch_height, _)| *max_epoch_height)
            .unwrap_or(0)
    }

    pub fn find_merkle_root(
        current_snapshots: &Vec<SnapshotInfo>, epoch_id: &EpochId,
    ) -> Option<MerkleHash> {
        current_snapshots
            .iter()
            .find(|i| i.get_snapshot_epoch_id() == epoch_id)
            .map(|i| i.merkle_root.clone())
    }

    pub fn wait_for_snapshot(
        &self, snapshot_epoch_id: &EpochId, try_open: bool,
        open_mpt_snapshot: bool,
    ) -> Result<
        Option<GuardedValue<RwLockReadGuard<Vec<SnapshotInfo>>, SnapshotDb>>,
    > {
        // Make sure that the snapshot info is ready at the same time of the
        // snapshot db. This variable is used for the whole scope
        // however prefixed with _ to please cargo fmt.
        let _snapshot_info_lock = self.snapshot_info_map_by_epoch.read();
        // maintain_snapshots_main_chain_confirmed() can not delete snapshot
        // while the current_snapshots are read locked.
        let guard = self.current_snapshots.read();
        match self.snapshot_manager.get_snapshot_by_epoch_id(
            snapshot_epoch_id,
            try_open,
            open_mpt_snapshot,
        )? {
            Some(snapshot_db) => {
                Ok(Some(GuardedValue::new(guard, snapshot_db)))
            }
            None => {
                drop(_snapshot_info_lock);
                drop(guard);
                // Wait for in progress snapshot.
                if let Some(in_progress_snapshot_task) = self
                    .in_progress_snapshotting_tasks
                    .read()
                    .get(snapshot_epoch_id)
                    .cloned()
                {
                    // Snapshotting error is thrown-out when the snapshot is
                    // first requested here.
                    if let Some(result) =
                        in_progress_snapshot_task.write().join()
                    {
                        result?;
                    }
                    let guard = self.current_snapshots.read();
                    match self.snapshot_manager.get_snapshot_by_epoch_id(
                        snapshot_epoch_id,
                        try_open,
                        open_mpt_snapshot,
                    ) {
                        Err(e) => Err(e),
                        Ok(None) => Ok(None),
                        Ok(Some(snapshot_db)) => {
                            Ok(Some(GuardedValue::new(guard, snapshot_db)))
                        }
                    }
                } else {
                    Ok(None)
                }
            }
        }
    }

    pub fn graceful_shutdown(&self) {
        // TODO: First cancel any ongoing thread join from
        // in_progress_snapshotting_joiner thread.
        self.in_progress_snapshot_finish_signaler
            .lock()
            .send(None)
            .ok();
        if let Some(joiner) = self.in_progress_snapshotting_joiner.lock().take()
        {
            joiner.join().ok();
        }
    }

    pub fn get_snapshot_manager(
        &self,
    ) -> &(dyn SnapshotManagerTrait<
        SnapshotDb = SnapshotDb,
        SnapshotDbManager = SnapshotDbManager,
    > + Send
             + Sync) {
        &*self.snapshot_manager
    }

    pub fn get_snapshot_epoch_count(&self) -> u32 {
        self.storage_conf.consensus_param.snapshot_epoch_count
    }

    pub fn get_snapshot_info_at_epoch(
        &self, snapshot_epoch_id: &EpochId,
    ) -> Option<SnapshotInfo> {
        self.snapshot_info_map_by_epoch
            .read()
            .get(snapshot_epoch_id)
            .map(Clone::clone)
    }

    pub fn latest_snapshot_epoch_height(&self) -> Option<u64> {
        self.snapshot_info_map_by_epoch
            .read()
            .get_map()
            .values()
            .map(|snapshot| snapshot.height)
            .max()
    }

    /// G-NET-1 — Lowest height we still have a snapshot for. The
    /// `NULL_EPOCH` genesis-window synthetic entry has height 0 and
    /// is excluded so peers see a meaningful retention floor (a real
    /// snapshot, not the genesis sentinel).
    pub fn earliest_snapshot_epoch_height(&self) -> Option<u64> {
        self.snapshot_info_map_by_epoch
            .read()
            .get_map()
            .iter()
            .filter(|(epoch_id, _)| **epoch_id != NULL_EPOCH)
            .map(|(_, snapshot)| snapshot.height)
            .min()
    }

    pub fn available_snapshot_count(&self) -> usize {
        self.snapshot_info_map_by_epoch.read().get_map().len()
    }

    /// G-NET-1 — Surface the retention-knob value to higher layers
    /// (RPC, heartbeat). See docs/flow-audit.md §5.8.
    pub fn keeps_pre_stable_snapshot(&self) -> bool {
        self.storage_conf.keep_snapshot_before_stable_checkpoint
    }

    pub fn get_delta_mpt(
        self: &Arc<Self>, snapshot_epoch_id: &EpochId,
    ) -> Result<Arc<DeltaMpt>> {
        {
            let snapshot_associated_mpts_locked =
                self.snapshot_associated_mpts_by_epoch.read();
            match snapshot_associated_mpts_locked.get(snapshot_epoch_id) {
                None => bail!(ErrorKind::DeltaMPTEntryNotFound),
                Some(delta_mpts) => {
                    if delta_mpts.1.is_some() {
                        return Ok(delta_mpts.1.as_ref().unwrap().clone());
                    }
                }
            }
        }

        StorageManager::new_or_get_delta_mpt(
            self.clone(),
            snapshot_epoch_id,
            &mut *self.snapshot_associated_mpts_by_epoch.write(),
        )
    }

    pub fn get_intermediate_mpt(
        &self, snapshot_epoch_id: &EpochId,
    ) -> Result<Option<Arc<DeltaMpt>>> {
        match self
            .snapshot_associated_mpts_by_epoch
            .read()
            .get(snapshot_epoch_id)
        {
            None => bail!(ErrorKind::DeltaMPTEntryNotFound),
            Some(mpts) => Ok(mpts.0.clone()),
        }
    }

    /// Return the existing delta mpt if the delta mpt already exists.
    pub fn new_or_get_delta_mpt(
        storage_manager: Arc<StorageManager>, snapshot_epoch_id: &EpochId,
        snapshot_associated_mpts_mut: &mut HashMap<
            EpochId,
            (Option<Arc<DeltaMpt>>, Option<Arc<DeltaMpt>>),
        >,
    ) -> Result<Arc<DeltaMpt>> {
        // Don't hold the lock while doing db io.
        // If the DeltaMpt already exists, the empty delta db creation should
        // fail already.

        let mut maybe_snapshot_entry =
            snapshot_associated_mpts_mut.get_mut(snapshot_epoch_id);
        if maybe_snapshot_entry.is_none() {
            bail!(ErrorKind::SnapshotNotFound);
        };
        // DeltaMpt already exists
        if maybe_snapshot_entry.as_ref().unwrap().1.is_some() {
            return Ok(maybe_snapshot_entry
                .unwrap()
                .1
                .as_ref()
                .unwrap()
                .clone());
        } else {
            let mpt_id = storage_manager.delta_mpts_id_gen.lock().allocate()?;
            let db_result = storage_manager
                .delta_mpt_open_db_lru
                .create(&snapshot_epoch_id, mpt_id);
            if db_result.is_err() {
                storage_manager.delta_mpts_id_gen.lock().free(mpt_id);
                db_result?;
            }
            let arc_delta_mpt = Arc::new(DeltaMpt::new(
                storage_manager.delta_mpt_open_db_lru.clone(),
                snapshot_epoch_id.clone(),
                storage_manager.clone(),
                mpt_id,
                storage_manager.delta_mpts_node_memory_manager.clone(),
            )?);

            maybe_snapshot_entry.as_mut().unwrap().1 =
                Some(arc_delta_mpt.clone());
            // For Genesis snapshot, the intermediate MPT is the same as the
            // delta MPT.
            if snapshot_epoch_id.eq(&NULL_EPOCH) {
                maybe_snapshot_entry.unwrap().0 = Some(arc_delta_mpt.clone());
            }

            return Ok(arc_delta_mpt);
        }
    }

    /// The methods clean up Delta DB when dropping an Delta MPT.
    /// It silently finishes and in case of error, it keeps the error
    /// and raise it later on.
    fn release_delta_mpt_actions_in_drop(
        &self, snapshot_epoch_id: &EpochId, delta_mpt_id: DeltaMptId,
    ) {
        debug!(
            "release_delta_mpt_actions_in_drop: snapshot_epoch_id: {:?}, delta_mpt_id: {}",
            snapshot_epoch_id, delta_mpt_id
        );
        self.delta_mpts_node_memory_manager
            .delete_mpt_from_cache(delta_mpt_id);
        self.delta_mpt_open_db_lru.release(delta_mpt_id, true);
        self.delta_mpts_id_gen.lock().free(delta_mpt_id);
        self.maybe_db_errors.set_maybe_error(
            self.delta_db_manager
                .destroy_delta_db(
                    &self.delta_db_manager.get_delta_db_name(snapshot_epoch_id),
                )
                .err(),
        );
    }

    fn release_delta_mpts_from_snapshot(
        &self,
        snapshot_associated_mpts_by_epoch: &mut HashMap<
            EpochId,
            (Option<Arc<DeltaMpt>>, Option<Arc<DeltaMpt>>),
        >,
        snapshot_epoch_id: &EpochId,
    ) -> Result<()> {
        // Release
        snapshot_associated_mpts_by_epoch.remove(snapshot_epoch_id);
        self.maybe_db_errors.take_result()
    }

    pub fn check_make_register_snapshot_background(
        this: Arc<Self>, snapshot_epoch_id: EpochId, height: u64,
        maybe_delta_db: Option<DeltaMptIterator>,
        recover_mpt_during_construct_main_state: bool,
    ) -> Result<()> {
        let this_cloned = this.clone();
        let mut in_progress_snapshotting_tasks =
            this_cloned.in_progress_snapshotting_tasks.write();

        let mut recover_mpt_with_kv_snapshot_exist = false;
        if !in_progress_snapshotting_tasks.contains_key(&snapshot_epoch_id)
            && this
                .snapshot_info_map_by_epoch
                .read()
                .get(&snapshot_epoch_id)
                .map_or(true, |info| {
                    if info.snapshot_info_kept_to_provide_sync
                        == SnapshotKeptToProvideSyncStatus::InfoOnly
                    {
                        true
                    } else {
                        recover_mpt_with_kv_snapshot_exist =
                            recover_mpt_during_construct_main_state;
                        recover_mpt_during_construct_main_state
                    }
                })
        {
            debug!(
                "start check_make_register_snapshot_background: epoch={:?} height={:?}",
                snapshot_epoch_id, height
            );

            let mut main_chain_parts = vec![
                Default::default();
                this.storage_conf.consensus_param.snapshot_epoch_count
                    as usize
            ];
            // Calculate main chain parts.
            let mut epoch_id = snapshot_epoch_id.clone();
            let mut delta_height =
                this.storage_conf.consensus_param.snapshot_epoch_count as usize
                    - 1;
            main_chain_parts[delta_height] = epoch_id.clone();
            // TODO Handle the special cases better
            let parent_snapshot_epoch_id = if maybe_delta_db.is_none() {
                // The case maybe_delta_db.is_none() means we are at height 0.
                // We set parent_snapshot of NULL to NULL, so that in
                // register_new_snapshot we will move the initial
                // delta_mpt to intermediate_mpt for NULL_EPOCH
                //
                NULL_EPOCH
            } else {
                let delta_db = maybe_delta_db.as_ref().unwrap();
                while delta_height > 0 {
                    epoch_id = match delta_db.mpt.get_parent_epoch(&epoch_id)? {
                        None => bail!(ErrorKind::DbValueError),
                        Some(epoch_id) => epoch_id,
                    };
                    delta_height -= 1;
                    main_chain_parts[delta_height] = epoch_id.clone();
                    trace!(
                        "check_make_register_snapshot_background: parent epoch_id={:?}",
                        epoch_id
                    );
                }
                if height
                    == this.storage_conf.consensus_param.snapshot_epoch_count
                        as u64
                {
                    // We need the case height == SNAPSHOT_EPOCHS_CAPACITY
                    // because the snapshot_info for genesis is
                    // stored in NULL_EPOCH. If we do not use the special case,
                    // it will be the epoch_id of genesis.
                    NULL_EPOCH
                } else {
                    delta_db.mpt.get_parent_epoch(&epoch_id)?.unwrap()
                }
            };

            let in_progress_snapshot_info = SnapshotInfo {
                snapshot_info_kept_to_provide_sync: Default::default(),
                serve_one_step_sync: true,
                height: height as u64,
                parent_snapshot_height: height
                    - this.storage_conf.consensus_param.snapshot_epoch_count
                        as u64,
                // This is unknown for now, and we don't care.
                merkle_root: Default::default(),
                parent_snapshot_epoch_id,
                main_chain_parts,
            };

            let parent_snapshot_epoch_id_cloned =
                in_progress_snapshot_info.parent_snapshot_epoch_id.clone();
            let mut in_progress_snapshot_info_cloned =
                in_progress_snapshot_info.clone();
            let task_finished_sender_cloned =
                this.in_progress_snapshot_finish_signaler.clone();

            // Cancel handle: shared between the task entry on
            // `in_progress_snapshotting_tasks` (so external code can
            // request cancellation via `InProgressSnapshotTask::request_cancel`)
            // and the background thread closure (so it can poll the
            // flag at well-defined boundaries — see Phase B.1 in
            // `docs/checkpoint-snapshot-lifecycle.md`).
            let cancel_requested = Arc::new(AtomicBool::new(false));
            let cancel_for_thread = Arc::clone(&cancel_requested);

            // Phase E — bump the in-flight gauge on spawn. The
            // corresponding decrement is at the end of the thread
            // closure (see the matching `snapshot_in_flight_adjust(-1)`
            // call before `task_result`).
            snapshot_in_flight_adjust(1);
            let thread_handle = thread::Builder::new()
                .name("Background Snapshotting".into()).spawn(move || {
                use std::sync::atomic::Ordering;
                let bg_start = std::time::Instant::now();
                let f = || -> Result<()> {
                    // Early-out: if cancellation requested before the
                    // merge starts, abort cleanly. The temp dir
                    // hasn't been created yet so nothing to clean.
                    if cancel_for_thread.load(Ordering::SeqCst) {
                        info!(
                            "Background Snapshotting: cancellation requested before merge start (epoch_id={:?})",
                            snapshot_epoch_id
                        );
                        return Ok(());
                    }
                    let (mut snapshot_info_map_locked, new_snapshot_info) = match maybe_delta_db {
                        None => {
                            in_progress_snapshot_info_cloned.merkle_root = MERKLE_NULL_NODE;
                            (this.snapshot_info_map_by_epoch.write(), in_progress_snapshot_info_cloned)
                        }
                        Some(delta_db) => {
                            this.snapshot_manager
                                .get_snapshot_db_manager()
                                .new_snapshot_by_merging(
                                    &parent_snapshot_epoch_id_cloned,
                                    snapshot_epoch_id.clone(), delta_db,
                                    in_progress_snapshot_info_cloned,
                                    &this.snapshot_info_map_by_epoch,
                                    height,
                                    recover_mpt_with_kv_snapshot_exist)?
                        }
                    };
                    // Post-merge cancellation gate. The merge runs to
                    // completion (we don't preempt mid-merge to avoid
                    // refactoring the merge internals), but if a fork
                    // was confirmed non-canonical while we were
                    // merging, we skip registration AND clean up the
                    // freshly-renamed snapshot directory so a phantom
                    // entry doesn't leak to peers. See Phase B.1 in
                    // docs/checkpoint-snapshot-lifecycle.md.
                    if cancel_for_thread.load(Ordering::SeqCst) {
                        info!(
                            "Background Snapshotting: cancellation requested mid-merge, discarding snapshot {:?}",
                            snapshot_epoch_id
                        );
                        // Drop the write lock before destroying so we
                        // don't hold it across the filesystem op.
                        drop(snapshot_info_map_locked);
                        let _ = this
                            .snapshot_manager
                            .get_snapshot_db_manager()
                            .destroy_snapshot(&snapshot_epoch_id);
                        SNAPSHOT_CANCELLED_TOTAL.mark(1);
                        return Ok(());
                    }

                    if let Err(e) = this.register_new_snapshot(new_snapshot_info.clone(), &mut snapshot_info_map_locked) {
                        // Phase E — distinguish register-time failure
                        // (C.1 / C.4 also bump their own dedicated
                        // counters; this catches everything else).
                        SNAPSHOT_FAILED_REGISTER_TOTAL.mark(1);
                        error!(
                            "Failed to register new snapshot {:?} {:?}.",
                            snapshot_epoch_id, new_snapshot_info
                        );
                        bail!(e);
                    }

                    task_finished_sender_cloned.lock().send(Some(snapshot_epoch_id))
                        .or(Err(Error::from(ErrorKind::MpscError)))?;
                    drop(snapshot_info_map_locked);

                    let debug_snapshot_checkers =
                        this.storage_conf.debug_snapshot_checker_threads;
                    for snapshot_checker in 0..debug_snapshot_checkers {
                        let begin_range =
                            (256 / debug_snapshot_checkers * snapshot_checker) as u8;
                        let end_range =
                            256 / debug_snapshot_checkers * (snapshot_checker + 1);
                        let end_range_excl = if end_range != 256 {
                            Some(vec![end_range as u8])
                        } else {
                            None
                        };
                        let this = this.clone();
                        thread::Builder::new().name(
                            format!("snapshot checker {} - {}", begin_range, end_range)).spawn(
                            move || -> Result<()> {
                                debug!(
                                    "Start snapshot checker {} of {}",
                                    snapshot_checker, debug_snapshot_checkers);
                                let snapshot_db = this.snapshot_manager
                                    .get_snapshot_by_epoch_id(
                                        &snapshot_epoch_id,
                                        /* try_open = */ false,
                                        true
                                    )?.unwrap();
                                let mut set_keys_iter =
                                    snapshot_db.dumped_delta_kv_set_keys_iterator()?;
                                let mut delete_keys_iter =
                                    snapshot_db.dumped_delta_kv_delete_keys_iterator()?;
                                let previous_snapshot_db = this.snapshot_manager
                                    .get_snapshot_by_epoch_id(
                                        &parent_snapshot_epoch_id_cloned,
                                        /* try_open = */ false,
                                        false
                                    )?.unwrap();
                                let mut previous_set_keys_iter = previous_snapshot_db
                                    .dumped_delta_kv_set_keys_iterator()?;
                                let mut previous_delete_keys_iter =
                                    previous_snapshot_db
                                        .dumped_delta_kv_delete_keys_iterator()?;

                                let mut checker_count = 0;

                                let set_iter = set_keys_iter.iter_range(
                                    &[begin_range],
                                    end_range_excl.as_ref().map(|v| &**v))?
                                    .take();
                                checker_count += check_key_value_load(&snapshot_db, set_iter, /* check_value = */ true)?;

                                let set_iter = previous_set_keys_iter.iter_range(
                                    &[begin_range], end_range_excl.as_ref().map(|v| &**v))?
                                    .take();
                                checker_count += check_key_value_load(&snapshot_db, set_iter, /* check_value = */ false)?;

                                let delete_iter = delete_keys_iter.iter_range(
                                    &[begin_range], end_range_excl.as_ref().map(|v| &**v))?
                                    .take();
                                checker_count += check_key_value_load(&snapshot_db, delete_iter, /* check_value = */ false)?;

                                let delete_iter = previous_delete_keys_iter.iter_range(
                                    &[begin_range], end_range_excl.as_ref().map(|v| &**v))?
                                    .take();
                                checker_count += check_key_value_load(&snapshot_db, delete_iter, /* check_value = */ false)?;

                                debug!(
                                    "Finished: snapshot checker {} of {}, {} keys",
                                    snapshot_checker, debug_snapshot_checkers, checker_count);
                                Ok(())
                            }
                        )?;
                    }

                    Ok(())
                };

                let task_result = f();
                // Phase E — record total wall time spent in this bg
                // thread (whether merge committed, cancelled, or
                // errored) and decrement the in-flight gauge. Failure
                // also bumps the dedicated `failed_total.merge_error`
                // counter for alerting.
                let elapsed_ms = bg_start.elapsed().as_millis() as usize;
                SNAPSHOT_CREATION_DURATION_MS_TOTAL.mark(elapsed_ms);
                if task_result.is_err() {
                    SNAPSHOT_FAILED_MERGE_TOTAL.mark(1);
                    warn!(
                        "Failed to create snapshot for epoch_id {:?} with error {:?}",
                        snapshot_epoch_id, task_result.as_ref().unwrap_err());
                }
                snapshot_in_flight_adjust(-1);

                task_result
            })?;

            in_progress_snapshotting_tasks.insert(
                snapshot_epoch_id,
                Arc::new(RwLock::new(InProgressSnapshotTask {
                    snapshot_info: in_progress_snapshot_info,
                    thread_handle: Some(thread_handle),
                    cancel_requested,
                })),
            );
        }

        Ok(())
    }

    /// This function is made public only for testing.
    pub fn register_new_snapshot(
        self: &Arc<Self>, new_snapshot_info: SnapshotInfo,
        snapshot_info_map_locked: &mut PersistedSnapshotInfoMap,
    ) -> Result<()> {
        debug!("register_new_snapshot: info={:?}", new_snapshot_info);
        let snapshot_epoch_id = new_snapshot_info.get_snapshot_epoch_id();

        // ----------------------------------------------------------------
        // C.1 — Merkle-root sanity check.
        //
        // A snapshot with `merkle_root == H256::zero()` is the result of
        // a default-initialised `SnapshotInfo` or a bug in
        // `new_snapshot_by_merging`. `MERKLE_NULL_NODE` is the legitimate
        // root for the genesis-window NULL_EPOCH parent path. Anything
        // else that's zero is a bug or corruption.
        //
        // Note: a deeper cross-check (against the consensus-layer
        // `StateRootWithAuxInfo` for this epoch) requires reaching the
        // BlockDataManager, which lives in a different crate. Tracked
        // as a follow-up enhancement — see
        // `docs/checkpoint-snapshot-lifecycle.md` Phase C.1 status
        // table.
        if new_snapshot_info.merkle_root == MerkleHash::default()
            && new_snapshot_info.merkle_root != MERKLE_NULL_NODE
        {
            SNAPSHOT_INVALID_ROOT_TOTAL.mark(1);
            error!(
                "register_new_snapshot REJECTED: snapshot {:?} has zero merkle_root and parent != NULL_EPOCH. \
                 Likely a default-initialised SnapshotInfo or a merge bug. info={:?}",
                snapshot_epoch_id, new_snapshot_info
            );
            bail!(ErrorKind::Msg(format!(
                "snapshot {:?} rejected: zero merkle_root with non-null parent",
                snapshot_epoch_id
            )));
        }

        // ----------------------------------------------------------------
        // C.4 — Parent-linkage validation.
        //
        // A snapshot whose parent isn't in `snapshot_info_map_by_epoch`
        // (and isn't the special NULL_EPOCH genesis-window parent) is
        // orphaned: its KV/MPT data has no usable delta-chain ancestor
        // and the snapshot can't be served to peers. Either parent
        // pruning raced ahead, or the producer mis-computed the parent
        // linkage. Either way, refuse the registration.
        if new_snapshot_info.parent_snapshot_epoch_id != NULL_EPOCH
            && snapshot_info_map_locked
                .get(&new_snapshot_info.parent_snapshot_epoch_id)
                .is_none()
        {
            SNAPSHOT_ORPHAN_REJECTED_TOTAL.mark(1);
            error!(
                "register_new_snapshot REJECTED: snapshot {:?} has parent {:?} which is not in snapshot_info_map_by_epoch. \
                 Either parent was pruned or producer mis-computed linkage. info={:?}",
                snapshot_epoch_id,
                new_snapshot_info.parent_snapshot_epoch_id,
                new_snapshot_info
            );
            bail!(ErrorKind::Msg(format!(
                "snapshot {:?} rejected: parent {:?} not registered",
                snapshot_epoch_id, new_snapshot_info.parent_snapshot_epoch_id
            )));
        }

        SNAPSHOT_REGISTERED_TOTAL.mark(1);
        // ----------------------------------------------------------------
        // Register intermediate MPT for the new snapshot.
        let mut snapshot_associated_mpts_locked =
            self.snapshot_associated_mpts_by_epoch.write();
        let in_recover_mode =
            snapshot_associated_mpts_locked.contains_key(snapshot_epoch_id);

        // Parent's delta mpt becomes intermediate_delta_mpt for the new
        // snapshot.
        //
        // It can't happen when the parent's delta mpt is still empty we
        // are already making the snapshot.
        //
        // But when we synced a new snapshot, the parent snapshot may not be
        // available at all, so when maybe_intermediate_delta_mpt is empty,
        // create it.
        let maybe_intermediate_delta_mpt = match snapshot_associated_mpts_locked
            .get(&new_snapshot_info.parent_snapshot_epoch_id)
        {
            None => {
                // The case when we synced a new snapshot and the parent
                // snapshot isn't available.
                snapshot_associated_mpts_locked.insert(
                    new_snapshot_info.parent_snapshot_epoch_id.clone(),
                    (None, None),
                );
                let parent_delta_mpt =
                    Some(StorageManager::new_or_get_delta_mpt(
                        self.clone(),
                        &new_snapshot_info.parent_snapshot_epoch_id,
                        &mut *snapshot_associated_mpts_locked,
                    )?);
                snapshot_associated_mpts_locked
                    .remove(&new_snapshot_info.parent_snapshot_epoch_id);

                parent_delta_mpt
            }
            Some(parent_snapshot_associated_mpts) => {
                if parent_snapshot_associated_mpts.1.is_none() {
                    debug!("MPT for parent_snapshot_epoch_id is none");
                    Some(StorageManager::new_or_get_delta_mpt(
                        self.clone(),
                        &new_snapshot_info.parent_snapshot_epoch_id,
                        &mut *snapshot_associated_mpts_locked,
                    )?)
                } else {
                    parent_snapshot_associated_mpts.1.clone()
                }
            }
        };
        let delta_mpt = if in_recover_mode {
            snapshot_associated_mpts_locked
                .get_mut(snapshot_epoch_id)
                // This is guaranteed in the in_recover_mode condition above.
                .unwrap()
                .1
                .take()
        } else {
            None
        };
        if !in_recover_mode || maybe_intermediate_delta_mpt.is_some() {
            snapshot_associated_mpts_locked.insert(
                snapshot_epoch_id.clone(),
                (maybe_intermediate_delta_mpt, delta_mpt),
            );
        }

        drop(snapshot_associated_mpts_locked);
        snapshot_info_map_locked
            .insert(snapshot_epoch_id, new_snapshot_info.clone())?;
        if !in_recover_mode {
            self.current_snapshots.write().push(new_snapshot_info);
        }

        Ok(())
    }

    pub fn maintain_state_confirmed<ConsensusInner: StateMaintenanceTrait>(
        &self, consensus_inner: &ConsensusInner, stable_checkpoint_height: u64,
        era_epoch_count: u64, confirmed_height: u64,
        state_availability_boundary: &RwLock<StateAvailabilityBoundary>,
    ) -> Result<()> {
        let additional_state_height_gap =
            (self.storage_conf.additional_maintained_snapshot_count
                * self.get_snapshot_epoch_count()) as u64;
        let maintained_state_height_lower_bound =
            if confirmed_height > additional_state_height_gap {
                confirmed_height - additional_state_height_gap
            } else {
                0
            };
        if maintained_state_height_lower_bound
            <= state_availability_boundary.read().lower_bound
        {
            return Ok(());
        }
        let maintained_epoch_id = consensus_inner
            .get_main_hash_from_epoch_number(
                maintained_state_height_lower_bound,
            )?;
        let maintained_epoch_execution_commitment = consensus_inner
            .get_epoch_execution_commitment_with_db(&maintained_epoch_id);
        let maintained_state_root = match &maintained_epoch_execution_commitment
        {
            Some(commitment) => &commitment.state_root_with_aux_info,
            None => return Ok(()),
        };

        self.maintain_snapshots_main_chain_confirmed(
            maintained_state_height_lower_bound,
            &maintained_epoch_id,
            maintained_state_root,
            state_availability_boundary,
            &|height, find_nearest_snapshot_multiple_of| {
                extra_snapshots_to_keep_predicate(
                    &self.storage_conf,
                    stable_checkpoint_height,
                    era_epoch_count,
                    height,
                    find_nearest_snapshot_multiple_of,
                )
            },
            stable_checkpoint_height,
        )
    }

    /// The algorithm figure out which snapshot to remove by simply going
    /// through all SnapshotInfo in one pass in the reverse order such that
    /// the parent snapshot is processed after the children snapshot.
    ///
    /// In the scan, main chain is traced from the confirmed snapshot. Whatever
    /// can't be traced shall be removed as non-main snapshot. Traced
    /// old main snapshot shall be deleted as well.
    ///
    /// Another maintenance of snapshots shall happen at Mazze start-up and
    /// after main chain is recognized.
    ///
    /// The behavior of old main snapshot deletion can be different between
    /// Archive Node and Full Node.
    pub fn maintain_snapshots_main_chain_confirmed(
        &self, maintained_state_height_lower_bound: u64,
        maintained_epoch_id: &EpochId,
        maintained_state_root: &StateRootWithAuxInfo,
        state_availability_boundary: &RwLock<StateAvailabilityBoundary>,
        extra_snapshots_to_keep: &dyn Fn(u64, &mut bool) -> bool,
        stable_checkpoint_height: u64,
    ) -> Result<()> {
        // Update the confirmed epoch id. Skip remaining actions when the
        // confirmed snapshot-able epoch id doesn't change
        {
            let mut last_confirmed_snapshottable_id_locked =
                self.last_confirmed_snapshottable_epoch_id.lock();
            if last_confirmed_snapshottable_id_locked.is_some() {
                if maintained_state_root.aux_info.intermediate_epoch_id.eq(
                    last_confirmed_snapshottable_id_locked.as_ref().unwrap(),
                ) {
                    return Ok(());
                }
            }
            *last_confirmed_snapshottable_id_locked = Some(
                maintained_state_root.aux_info.intermediate_epoch_id.clone(),
            );
        }

        let confirmed_intermediate_height = maintained_state_height_lower_bound
            - StateIndex::height_to_delta_height(
                maintained_state_height_lower_bound,
                self.get_snapshot_epoch_count(),
            ) as u64;

        let confirmed_snapshot_height = if confirmed_intermediate_height
            > self.get_snapshot_epoch_count() as u64
        {
            confirmed_intermediate_height
                - self.get_snapshot_epoch_count() as u64
        } else {
            0
        };
        let first_available_state_height = if confirmed_snapshot_height > 0 {
            confirmed_snapshot_height + 1
        } else {
            0
        };

        debug!(
            "maintain_snapshots_main_chain_confirmed: confirmed_height {}, \
             confirmed_epoch_id {:?}, confirmed_intermediate_id {:?}, \
             confirmed_snapshot_id {:?}, confirmed_intermediate_height {}, \
             confirmed_snapshot_height {}, first_available_state_height {}",
            maintained_state_height_lower_bound,
            maintained_epoch_id,
            maintained_state_root.aux_info.intermediate_epoch_id,
            maintained_state_root.aux_info.snapshot_epoch_id,
            confirmed_intermediate_height,
            confirmed_snapshot_height,
            first_available_state_height,
        );
        let mut extra_snapshot_infos_kept_for_sync = vec![];
        let mut non_main_snapshots_to_remove = HashSet::new();
        let mut old_main_snapshots_to_remove = vec![];
        // We will keep some extra snapshots to provide sync. For any snapshot
        // to keep, we must keep all snapshot_info from the main tip to
        // the snapshot, so that in the next run the snapshot is still
        // recognized as "old main".
        let mut old_main_snapshot_infos_to_remove = vec![];
        let mut find_nearest_multiple_of = false;
        let mut in_progress_snapshot_to_cancel = vec![];

        {
            let current_snapshots = self.current_snapshots.read();

            let mut prev_snapshot_epoch_id = &NULL_EPOCH;

            // Check snapshots which has height lower than confirmed_height
            for snapshot_info in current_snapshots.iter().rev() {
                let snapshot_epoch_id = snapshot_info.get_snapshot_epoch_id();
                if snapshot_info.height == confirmed_snapshot_height {
                    // Remove all non-main Snapshot at
                    // confirmed_snapshot_height
                    if snapshot_epoch_id
                        .eq(&maintained_state_root.aux_info.snapshot_epoch_id)
                    {
                        prev_snapshot_epoch_id =
                            &snapshot_info.parent_snapshot_epoch_id;
                    } else {
                        non_main_snapshots_to_remove
                            .insert(snapshot_epoch_id.clone());
                    }
                } else if snapshot_info.height < confirmed_snapshot_height {
                    // We remove for older main snapshot one after another.
                    if snapshot_epoch_id.eq(prev_snapshot_epoch_id) {
                        if extra_snapshots_to_keep(
                            snapshot_info.height,
                            &mut find_nearest_multiple_of,
                        ) {
                            // For any snapshot to keep, we keep all snapshot
                            // infos from main tip to it.
                            for snapshot_epoch_id_to_keep_info in std::mem::take(
                                &mut old_main_snapshot_infos_to_remove,
                            ) {
                                extra_snapshot_infos_kept_for_sync.push((
                                    snapshot_epoch_id_to_keep_info,
                                    SnapshotKeptToProvideSyncStatus::InfoOnly,
                                ));
                            }
                            extra_snapshot_infos_kept_for_sync
                                .push((snapshot_epoch_id.clone(), SnapshotKeptToProvideSyncStatus::InfoAndSnapshot));
                        } else {
                            // Retain the snapshot information for the one
                            // preceding the stable checkpoint
                            if snapshot_info.height
                                + self
                                    .storage_conf
                                    .consensus_param
                                    .snapshot_epoch_count
                                    as u64
                                != stable_checkpoint_height
                            {
                                old_main_snapshot_infos_to_remove
                                    .push(snapshot_epoch_id.clone());
                            }
                            old_main_snapshots_to_remove
                                .push(snapshot_epoch_id.clone());
                        }
                        prev_snapshot_epoch_id =
                            &snapshot_info.parent_snapshot_epoch_id;
                    } else {
                        // Any other snapshot with higher height is non-main.
                        non_main_snapshots_to_remove
                            .insert(snapshot_epoch_id.clone());
                    }
                } else if snapshot_info.height
                    < maintained_state_height_lower_bound
                {
                    // There can be at most 1 snapshot between the snapshot at
                    // confirmed_snapshot_height and confirmed_height.
                    //
                    // When a snapshot has height > confirmed_snapshot_height,
                    // but doesn't contain confirmed_state_root.aux_info.
                    // intermediate_epoch_id, it must be a non-main fork.
                    if snapshot_info
                        .get_epoch_id_at_height(confirmed_intermediate_height)
                        != Some(
                            &maintained_state_root
                                .aux_info
                                .intermediate_epoch_id,
                        )
                    {
                        debug!(
                            "remove mismatch intermediate snapshot: {:?}",
                            snapshot_info.get_epoch_id_at_height(
                                confirmed_intermediate_height
                            )
                        );
                        non_main_snapshots_to_remove
                            .insert(snapshot_epoch_id.clone());
                    }
                }
            }

            debug!(
                "finished scanning for lower snapshots: \
                 old_main_snapshots_to_remove {:?}, \
                 old_main_snapshot_infos_to_remove {:?}, \
                 non_main_snapshots_to_remove {:?}",
                old_main_snapshots_to_remove,
                old_main_snapshot_infos_to_remove,
                non_main_snapshots_to_remove
            );

            // Check snapshots which has height >= confirmed_height
            for snapshot_info in &*current_snapshots {
                // Check for non-main snapshot to remove.
                match snapshot_info
                    .get_epoch_id_at_height(maintained_state_height_lower_bound)
                {
                    Some(path_epoch_id) => {
                        // Check if the snapshot is within
                        // confirmed_epoch's
                        // subtree.
                        if path_epoch_id != maintained_epoch_id {
                            debug!(
                                "remove non-subtree snapshot {:?}, got {:?}, expected {:?}",
                                snapshot_info.get_snapshot_epoch_id(),
                                path_epoch_id, maintained_epoch_id,
                            );
                            non_main_snapshots_to_remove.insert(
                                snapshot_info.get_snapshot_epoch_id().clone(),
                            );
                        }
                    }
                    None => {
                        // The snapshot is so deep that we have to check its
                        // parent to see if it's within confirmed_epoch's
                        // subtree.
                        if non_main_snapshots_to_remove
                            .contains(&snapshot_info.parent_snapshot_epoch_id)
                        {
                            debug!(
                                "remove non-subtree deep snapshot {:?}, parent_snapshot_epoch_id {:?}",
                                snapshot_info.get_snapshot_epoch_id(),
                                snapshot_info.parent_snapshot_epoch_id
                            );
                            // The snapshot may already exist. This is why we
                            // must use HashSet for
                            // non_main_snapshots_to_remove.
                            non_main_snapshots_to_remove.insert(
                                snapshot_info.get_snapshot_epoch_id().clone(),
                            );
                        }
                    }
                }
            }
        }

        for (in_progress_epoch_id, in_progress_snapshot_task) in
            &*self.in_progress_snapshotting_tasks.read()
        {
            let mut to_cancel = false;
            let in_progress_snapshot_info =
                &in_progress_snapshot_task.read().snapshot_info;

            // The logic is similar as above for snapshot deletion.
            if in_progress_snapshot_info.height < confirmed_intermediate_height
            {
                to_cancel = true;
            } else if in_progress_snapshot_info.height
                < maintained_state_height_lower_bound
            {
                if in_progress_snapshot_info
                    .get_epoch_id_at_height(confirmed_intermediate_height)
                    != Some(
                        &maintained_state_root.aux_info.intermediate_epoch_id,
                    )
                {
                    to_cancel = true;
                }
            } else {
                match in_progress_snapshot_info
                    .get_epoch_id_at_height(maintained_state_height_lower_bound)
                {
                    Some(path_epoch_id) => {
                        if path_epoch_id != maintained_epoch_id {
                            to_cancel = true;
                        }
                    }
                    None => {
                        if non_main_snapshots_to_remove.contains(
                            &in_progress_snapshot_info.parent_snapshot_epoch_id,
                        ) {
                            to_cancel = true;
                        }
                    }
                }
            }

            if to_cancel {
                in_progress_snapshot_to_cancel
                    .push(in_progress_epoch_id.clone())
            }
        }

        let mut non_main_snapshots_to_remove =
            non_main_snapshots_to_remove.drain().collect();
        // Update snapshot_infos and filter out already removed snapshots from
        // the removal lists.
        {
            let mut info_maps = self.snapshot_info_map_by_epoch.write();
            let removal_filter = |vec: &mut Vec<EpochId>| {
                vec.retain(|epoch| {
                    info_maps.get(epoch).map_or(true, |info| {
                        // The snapshot itself is already removed.
                        info.snapshot_info_kept_to_provide_sync
                            != SnapshotKeptToProvideSyncStatus::InfoOnly
                    })
                })
            };
            removal_filter(&mut non_main_snapshots_to_remove);
            removal_filter(&mut old_main_snapshots_to_remove);

            let mut updated_snapshot_info_epochs =
                HashMap::<EpochId, SnapshotKeptToProvideSyncStatus>::default();
            for (epoch, new_status) in &extra_snapshot_infos_kept_for_sync {
                if let Some(info) = info_maps.get(epoch) {
                    if info.snapshot_info_kept_to_provide_sync != *new_status {
                        let mut new_snapshot_info = info.clone();
                        new_snapshot_info.snapshot_info_kept_to_provide_sync =
                            *new_status;
                        info_maps.insert(epoch, new_snapshot_info)?;
                        updated_snapshot_info_epochs
                            .insert(*epoch, *new_status);
                    }
                }
            }
            if updated_snapshot_info_epochs.len() > 0 {
                let mut current_snapshots = self.current_snapshots.write();
                for snapshot_info in current_snapshots.iter_mut() {
                    if let Some(new_status) = updated_snapshot_info_epochs
                        .get(&snapshot_info.get_snapshot_epoch_id())
                    {
                        snapshot_info.snapshot_info_kept_to_provide_sync =
                            *new_status;
                    }
                }
            }
        }
        // §D.3 prevention — protect the ancestor chain of every surviving
        // snapshot from pruning. Aggressive retention (full-fast keeps the
        // bare minimum) could otherwise remove a pre-stable parent while its
        // child survives, breaking the snapshot delta chain. That orphan is
        // harmless while the node runs but trips the D.3 startup invariant on
        // the next restart — observed live crashing the mining leader and
        // halting the chain. Here we walk up from each surviving snapshot and
        // drop any ancestor that pruning slated for removal. Pure filtering:
        // it only ever RETAINS more snapshots, never removes additional ones,
        // so it cannot make the delta chain worse. The D.3 self-heal in
        // `load`/startup is the backstop for any orphan predating this fix.
        {
            let removing: HashSet<EpochId> = old_main_snapshots_to_remove
                .iter()
                .chain(non_main_snapshots_to_remove.iter())
                .cloned()
                .collect();
            let current_snapshots = self.current_snapshots.read();
            let parent_of: HashMap<EpochId, EpochId> = current_snapshots
                .iter()
                .map(|s| {
                    (
                        s.get_snapshot_epoch_id().clone(),
                        s.parent_snapshot_epoch_id.clone(),
                    )
                })
                .collect();
            let mut protected = HashSet::new();
            for s in current_snapshots.iter() {
                if removing.contains(s.get_snapshot_epoch_id()) {
                    continue; // being removed; not a survivor to protect for
                }
                // Walk the full ancestor chain so protection is transitive.
                let mut p = s.parent_snapshot_epoch_id.clone();
                while p != NULL_EPOCH {
                    if removing.contains(&p) {
                        protected.insert(p.clone());
                    }
                    match parent_of.get(&p) {
                        Some(next) => p = next.clone(),
                        None => break,
                    }
                }
            }
            drop(current_snapshots);
            if !protected.is_empty() {
                warn!(
                    "D.3 prevention: retaining {} ancestor snapshot(s) that \
                     pruning would have orphaned (keeps the delta chain intact \
                     across restart)",
                    protected.len()
                );
                old_main_snapshots_to_remove
                    .retain(|e| !protected.contains(e));
                non_main_snapshots_to_remove
                    .retain(|e| !protected.contains(e));
                old_main_snapshot_infos_to_remove
                    .retain(|e| !protected.contains(e));
            }
        }

        if !non_main_snapshots_to_remove.is_empty()
            || !old_main_snapshots_to_remove.is_empty()
        {
            {
                // TODO: Archive node may do something different.
                let state_boundary = &mut *state_availability_boundary.write();
                if first_available_state_height > state_boundary.lower_bound {
                    state_boundary
                        .adjust_lower_bound(first_available_state_height);
                }
            }

            self.remove_snapshots(
                &old_main_snapshots_to_remove,
                &non_main_snapshots_to_remove,
                &old_main_snapshot_infos_to_remove
                    .iter()
                    .chain(non_main_snapshots_to_remove.iter())
                    .cloned()
                    .collect(),
            )?;
        }

        // Cancellation of in-flight snapshots whose targets have been
        // confirmed non-canonical. We *signal* cancellation via the
        // per-task `cancel_requested` flag; the background thread
        // checks it before merge-start and again before registration
        // and self-cleans the temp/renamed snapshot dir. The thread
        // is **not** joined here — that would block this maintenance
        // call indefinitely.
        //
        // See Phase B.1 in docs/checkpoint-snapshot-lifecycle.md.
        if !in_progress_snapshot_to_cancel.is_empty() {
            let in_progress_snapshotting_locked =
                self.in_progress_snapshotting_tasks.read();
            for epoch_id in &in_progress_snapshot_to_cancel {
                if let Some(task) = in_progress_snapshotting_locked.get(epoch_id) {
                    task.read().request_cancel();
                    info!(
                        "maintain_snapshots_main_chain_confirmed: requested cancellation for in-progress snapshot {:?}",
                        epoch_id
                    );
                }
            }
        }

        info!("maintain_snapshots_main_chain_confirmed: finished");
        Ok(())
    }

    fn remove_snapshots(
        &self, old_main_snapshots_to_remove: &[EpochId],
        non_main_snapshots_to_remove: &[EpochId],
        snapshot_infos_to_remove: &HashSet<EpochId>,
    ) -> Result<()> {
        let mut current_snapshots_locked = self.current_snapshots.write();
        current_snapshots_locked.retain(|x| {
            !snapshot_infos_to_remove.contains(x.get_snapshot_epoch_id())
        });
        info!(
            "maintain_snapshots_main_chain_confirmed: remove the following snapshot infos {:?}",
            snapshot_infos_to_remove,
        );
        for snapshot_epoch_id in old_main_snapshots_to_remove {
            self.snapshot_manager
                .remove_old_main_snapshot(&snapshot_epoch_id)?;
            // Phase E — retention-driven prune of an old main snapshot.
            SNAPSHOT_PRUNED_TOTAL.mark(1);
        }
        for snapshot_epoch_id in non_main_snapshots_to_remove {
            self.snapshot_manager
                .remove_non_main_snapshot(&snapshot_epoch_id)?;
            // Phase E — non-main (forked-off) snapshot pruned.
            SNAPSHOT_PRUNED_TOTAL.mark(1);
        }

        drop(current_snapshots_locked);
        unsafe {
            let mut snapshot_info_map = self.snapshot_info_map_by_epoch.write();
            for snapshot_epoch_id in snapshot_infos_to_remove {
                snapshot_info_map.remove_in_mem_only(snapshot_epoch_id);
            }
        }
        {
            let snapshot_associated_mpts_by_epoch_locked =
                &mut *self.snapshot_associated_mpts_by_epoch.write();

            for snapshot_epoch_id in old_main_snapshots_to_remove
                .iter()
                .chain(non_main_snapshots_to_remove.iter())
            {
                self.release_delta_mpts_from_snapshot(
                    snapshot_associated_mpts_by_epoch_locked,
                    snapshot_epoch_id,
                )?
            }
        }
        {
            // Only remove snapshot_info from db when no exception have
            // happened.
            let mut snapshot_info_map_by_epoch =
                self.snapshot_info_map_by_epoch.write();
            for snapshot_epoch_id in snapshot_infos_to_remove {
                snapshot_info_map_by_epoch.remove(&snapshot_epoch_id)?;
            }
        }

        Ok(())
    }

    /// Returns the hot-tier MDBX environment if the node was
    /// configured with `state_db_backend = Mdbx` (the default).
    /// Returns `None` when the operator pinned to ParityDB for the
    /// rollout fallback.
    ///
    /// Consumers — primarily the executor's live-state cache and revm's
    /// `MazzeDatabase` adapter — use this handle to open per-column
    /// `KvdbMdbx` views for fast account/storage reads. See
    /// docs/storage-architecture.md §3 for the planned column layout.
    pub fn mdbx_env(
        &self,
    ) -> Option<Arc<crate::impls::storage_db::kvdb_mdbx::MdbxEnv>> {
        self.mdbx_env.clone()
    }

    /// Handle to the dedicated snapshot MDBX env — the growth-
    /// enabled cold tier at `storage_db/mdbx_snapshot/`. Returns
    /// `None` iff the node was pinned to `ParityDb` (a rollout
    /// escape hatch already rejected in `new_arc` — kept `Option`
    /// for parallel shape with `mdbx_env`). Consumed by
    /// `SnapshotDbManagerMdbx` in the Phase 5c main commit.
    pub fn snapshot_mdbx_env(
        &self,
    ) -> Option<Arc<crate::impls::storage_db::kvdb_mdbx::MdbxEnv>> {
        self.snapshot_mdbx_env.clone()
    }

    /// Sample the snapshot env's occupancy and republish it into
    /// the `mdbx_snapshot.bytes_used` and
    /// `mdbx_snapshot.map_pct_used_x100` gauges. Emits a `warn!`
    /// when occupancy crosses [`SNAPSHOT_MDBX_CAPACITY_WARN_PCT`]
    /// so operators can raise `snapshot_mdbx_max_mb` in hydra.toml
    /// before hitting `MDBX_MAP_FULL`.
    ///
    /// Cheap: one `mdbx_stat` call on col-0. Called at each
    /// [`Self::log_usage`] tick.
    pub fn sample_snapshot_mdbx_capacity(&self) {
        let env = match &self.snapshot_mdbx_env {
            Some(e) => Arc::clone(e),
            None => return,
        };
        // The snapshot env owns col-0; other columns are reserved
        // in the design doc but not yet allocated, so col-0 stats
        // is the whole env's disk footprint today.
        let kvdb = crate::impls::storage_db::kvdb_mdbx::KvdbMdbx
            ::with_column(env, 0);
        let stats = match kvdb.stats() {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    "sample_snapshot_mdbx_capacity: stats() failed: {}",
                    e
                );
                return;
            }
        };
        SNAPSHOT_MDBX_BYTES_USED.update(stats.bytes_used as usize);
        let Some(pct_x100) = snapshot_mdbx_pct_x100(
            stats.bytes_used,
            self.snapshot_mdbx_max_bytes,
        ) else {
            return; // max_bytes == 0: gauge left untouched.
        };
        SNAPSHOT_MDBX_MAP_PCT_USED_X100.update(pct_x100 as usize);
        let pct = pct_x100 / 100;
        if pct >= SNAPSHOT_MDBX_CAPACITY_WARN_PCT {
            warn!(
                "mdbx_snapshot capacity at {}.{:02}% ({} / {} \
                 MB) — raise `snapshot_mdbx_max_mb` in hydra.toml \
                 and restart before hitting MDBX_MAP_FULL.",
                pct,
                pct_x100 % 100,
                stats.bytes_used / (1024 * 1024),
                self.snapshot_mdbx_max_bytes / (1024 * 1024)
            );
        }
    }

    pub fn log_usage(&self) {
        // Refresh the snapshot-env capacity gauge at the same
        // cadence as other storage-usage logging — cheap `mdbx_stat`
        // on col-0.
        self.sample_snapshot_mdbx_capacity();

        let mut delta_mpts = HashMap::new();
        for (_snapshot_epoch_id, associated_delta_mpts) in
            &*self.snapshot_associated_mpts_by_epoch.read()
        {
            if let Some(delta_mpt) = associated_delta_mpts.0.as_ref() {
                delta_mpts.insert(delta_mpt.get_mpt_id(), delta_mpt.clone());
            }
            if let Some(delta_mpt) = associated_delta_mpts.1.as_ref() {
                delta_mpts.insert(delta_mpt.get_mpt_id(), delta_mpt.clone());
            }
        }
        if let Some((_mpt_id, delta_mpt)) = delta_mpts.iter().next() {
            delta_mpt.log_usage();

            // Now delta_mpt calls log_usage of the singleton
            // node_memory_manager, so there is no need to log_usage
            // on second delta_mpt.
        }
    }

    pub fn load_persist_state(self: &Arc<Self>) -> Result<()> {
        let snapshot_info_map = &mut *self.snapshot_info_map_by_epoch.write();

        // Always keep the information for genesis snapshot.
        self.snapshot_associated_mpts_by_epoch
            .write()
            .insert(NULL_EPOCH, (None, None));
        snapshot_info_map
            .insert(&NULL_EPOCH, SnapshotInfo::genesis_snapshot_info())?;
        self.current_snapshots
            .write()
            .push(SnapshotInfo::genesis_snapshot_info());

        // Persist state loaded.
        let snapshot_persist_state = self
            .snapshot_manager
            .get_snapshot_db_manager()
            .scan_persist_state(snapshot_info_map.get_map())?;

        debug!("snapshot persist state {:?}", snapshot_persist_state);

        *self.persist_state_from_initialization.write() = Some((
            snapshot_persist_state.temp_snapshot_db_existing,
            snapshot_persist_state.removed_snapshots,
            snapshot_persist_state.max_epoch_height,
            snapshot_persist_state.max_snapshot_epoch_height_has_mpt,
        ));
        self.snapshot_manager
            .get_snapshot_db_manager()
            .update_latest_snapshot_id(
                snapshot_persist_state.max_epoch_id,
                snapshot_persist_state.max_epoch_height,
            );

        // Remove missing snapshots.
        for snapshot_epoch_id in snapshot_persist_state.missing_snapshots {
            if snapshot_epoch_id == NULL_EPOCH {
                continue;
            }
            // Remove the delta mpt if the snapshot is missing.
            self.delta_db_manager
                .destroy_delta_db(
                    &self
                        .delta_db_manager
                        .get_delta_db_name(&snapshot_epoch_id),
                )
                .or_else(|e| match e.kind() {
                    ErrorKind::Io(io_err) => match io_err.kind() {
                        std::io::ErrorKind::NotFound => Ok(()),
                        _ => Err(e),
                    },
                    _ => Err(e),
                })?;
            snapshot_info_map.remove(&snapshot_epoch_id)?;
        }

        let (missing_delta_db_snapshots, delta_dbs) = self
            .delta_db_manager
            .scan_persist_state(snapshot_info_map.get_map())?;

        let mut delta_mpts = HashMap::new();
        for (snapshot_epoch_id, delta_db) in delta_dbs {
            let mpt_id = self.delta_mpts_id_gen.lock().allocate()?;
            self.delta_mpt_open_db_lru.import(
                &snapshot_epoch_id,
                mpt_id,
                delta_db,
            )?;
            delta_mpts.insert(
                snapshot_epoch_id.clone(),
                Arc::new(DeltaMpt::new(
                    self.delta_mpt_open_db_lru.clone(),
                    snapshot_epoch_id.clone(),
                    self.clone(),
                    mpt_id,
                    self.delta_mpts_node_memory_manager.clone(),
                )?),
            );
        }

        for snapshot_epoch_id in missing_delta_db_snapshots {
            if snapshot_epoch_id == NULL_EPOCH {
                continue;
            }
            // Do not remove a snapshot which has intermediate delta mpt,
            // because it could be a freshly made snapshot before the previous
            // shutdown. A freshly made snapshot does not have delta db yet.
            if let Some(snapshot_info) =
                snapshot_info_map.get(&snapshot_epoch_id)
            {
                if delta_mpts
                    .contains_key(&snapshot_info.parent_snapshot_epoch_id)
                {
                    continue;
                }
            }
            error!(
                "Missing intermediate mpt and delta mpt for snapshot {:?}",
                snapshot_epoch_id
            );
            snapshot_info_map.remove(&snapshot_epoch_id)?;
            self.snapshot_manager
                .get_snapshot_db_manager()
                .destroy_snapshot(&snapshot_epoch_id)?;
        }

        // Restore current_snapshots.
        let mut snapshots = snapshot_info_map
            .get_map()
            .iter()
            .map(|(_, snapshot_info)| snapshot_info.clone())
            .collect::<Vec<_>>();
        snapshots.sort_by(|x, y| x.height.partial_cmp(&y.height).unwrap());

        // ----------------------------------------------------------------
        // D.3 — Startup invariant on the snapshot delta chain.
        //
        // Every retained snapshot whose parent is NOT `NULL_EPOCH` must
        // have that parent still present in `snapshot_info_map_by_epoch`.
        // If the parent is missing, the delta chain is broken: the
        // executor can't apply forward epochs because it has no
        // ancestor state to deltas-against. This typically happens
        // when a previous run with
        // `keep_snapshot_before_stable_checkpoint = false` pruned the
        // pre-stable parent snapshot while its child is still retained
        // (see `extra_snapshots_to_keep_predicate` in this file).
        //
        // Refuse to start instead of crashing later with an opaque
        // executor error. Operator action is in the bail message.
        // See `docs/checkpoint-snapshot-lifecycle.md` Phase D.3.
        // §D.3 self-heal — auto-repair instead of refusing to start. A
        // snapshot whose parent is missing has a broken delta chain (the
        // executor can't apply forward epochs without the parent's state).
        // Rather than `bail!` and wedge the node on every subsequent restart,
        // delete the orphan (and, transitively, any child that depended on
        // it) and continue — the missing state is rebuilt via catch-up
        // replay. Iterating in ascending height order means removing an
        // orphan from `snapshot_info_map` immediately flags its child on a
        // later iteration (the child's parent is now gone from the map), so
        // cascades resolve in a single pass. This is exactly the "delete the
        // orphan snapshot" operator recovery the old bail message documented,
        // done automatically. Root cause of the orphan is aggressive snapshot
        // pruning removing a pre-stable parent while a child is retained (see
        // the parent-protection guard in
        // `maintain_snapshots_main_chain_confirmed`); this heal is the safety
        // net. Observed live: a full-fast node that pruned a pre-stable parent
        // while running crashed on its next restart, taking down the mining
        // leader and halting the chain.
        let mut healed_orphans = HashSet::new();
        for snapshot_info in snapshots.iter() {
            let parent_id = &snapshot_info.parent_snapshot_epoch_id;
            if *parent_id != NULL_EPOCH
                && snapshot_info_map.get(parent_id).is_none()
            {
                let snapshot_id =
                    snapshot_info.get_snapshot_epoch_id().clone();
                warn!(
                    "D.3 self-heal: retained snapshot {:?} (height {}) has \
                     missing parent {:?}; deleting the orphan and continuing \
                     (catch-up replay will be longer). Likely cause: \
                     aggressive snapshot pruning removed the pre-stable \
                     parent while the child was still retained.",
                    snapshot_id, snapshot_info.height, parent_id,
                );
                snapshot_info_map.remove(&snapshot_id)?;
                self.snapshot_manager
                    .get_snapshot_db_manager()
                    .destroy_snapshot(&snapshot_id)?;
                healed_orphans.insert(snapshot_id);
            }
        }
        if !healed_orphans.is_empty() {
            snapshots.retain(|s| {
                !healed_orphans.contains(s.get_snapshot_epoch_id())
            });
        }

        let current_snapshots = &mut *self.current_snapshots.write();
        *current_snapshots = snapshots;

        let snapshot_associated_mpts =
            &mut *self.snapshot_associated_mpts_by_epoch.write();
        for snapshot_info in current_snapshots {
            snapshot_associated_mpts.insert(
                snapshot_info.get_snapshot_epoch_id().clone(),
                (
                    delta_mpts
                        .get(&snapshot_info.parent_snapshot_epoch_id)
                        .map(|x| x.clone()),
                    delta_mpts
                        .get(snapshot_info.get_snapshot_epoch_id())
                        .map(|x| x.clone()),
                ),
            );
        }

        Ok(())
    }
}

fn extra_snapshots_to_keep_predicate(
    storage_conf: &StorageConfiguration, stable_checkpoint_height: u64,
    era_epoch_count: u64, height: u64,
    find_epoch_nearest_multiple_of: &mut bool,
) -> bool {
    for conf in &storage_conf.provide_more_snapshot_for_sync {
        match conf {
            ProvideExtraSnapshotSyncConfig::StableCheckpoint => {
                if height >= stable_checkpoint_height
                    && (height - stable_checkpoint_height) % era_epoch_count
                        == 0
                {
                    return true;
                }
                // The bound_height ensures that the snapshot before
                // stable_genesis will not be removed, so that
                // the execution of the epochs following
                // stable_genesis can go through a normal path where both
                // snapshot and intermediate delta mpt exist.
                //
                // The historical corner case here — operators setting
                // `keep_snapshot_before_stable_checkpoint = false` and
                // ending up with an unexecutable orphan child — is now
                // guarded against by D.3's startup invariant in
                // `load_persist_state`: if pruning ever produces a
                // child whose parent is gone, the node refuses to
                // start with an actionable error pointing at this
                // config knob. See
                // `docs/checkpoint-snapshot-lifecycle.md` Phase D.3.
                let check_next_snapshot_height = height
                    + (storage_conf.consensus_param.snapshot_epoch_count
                        as u64);
                if (check_next_snapshot_height >= stable_checkpoint_height)
                    && (check_next_snapshot_height - stable_checkpoint_height)
                        % era_epoch_count
                        == 0
                {
                    return storage_conf.keep_snapshot_before_stable_checkpoint;
                }

                if storage_conf.keep_era_genesis_snapshot {
                    let era_genesis_snapshot_height =
                        if stable_checkpoint_height
                            >= storage_conf.consensus_param.era_epoch_count
                        {
                            stable_checkpoint_height
                                - storage_conf.consensus_param.era_epoch_count
                        } else {
                            0
                        };

                    if era_genesis_snapshot_height == height {
                        return true;
                    }
                }
            }
            ProvideExtraSnapshotSyncConfig::EpochNearestMultipleOf(
                multiple,
            ) => {
                if *find_epoch_nearest_multiple_of
                    && height % (*multiple as u64) == 0
                {
                    *find_epoch_nearest_multiple_of = false;
                    return true;
                }
            }
        }
    }
    false
}

struct MaybeDeltaTrieDestroyErrors {
    delta_trie_destroy_error_1: Cell<Option<Error>>,
    delta_trie_destroy_error_2: Cell<Option<Error>>,
}

// It's only used when relevant lock has been acquired.
unsafe impl Sync for MaybeDeltaTrieDestroyErrors {}

impl MaybeDeltaTrieDestroyErrors {
    fn new() -> Self {
        Self {
            delta_trie_destroy_error_1: Cell::new(None),
            delta_trie_destroy_error_2: Cell::new(None),
        }
    }

    fn set_maybe_error(&self, e: Option<Error>) {
        self.delta_trie_destroy_error_2
            .replace(self.delta_trie_destroy_error_1.replace(e));
    }

    fn take_result(&self) -> Result<()> {
        let e1 = self.delta_trie_destroy_error_1.take().map(|e| Box::new(e));
        let e2 = self.delta_trie_destroy_error_2.take().map(|e| Box::new(e));
        if e1.is_some() || e2.is_some() {
            Err(ErrorKind::DeltaMPTDestroyErrors(e1, e2).into())
        } else {
            Ok(())
        }
    }
}

use crate::{
    impls::{
        delta_mpt::{
            node_memory_manager::{
                DeltaMptsCacheAlgorithm, DeltaMptsNodeMemoryManager,
            },
            node_ref_map::DeltaMptId,
        },
        errors::*,
        state_manager::{DeltaDbManager, SnapshotDb, SnapshotDbManager},
        storage_db::snapshot_debug::check_key_value_load,
        storage_manager::snapshot_manager::SnapshotManager,
    },
    snapshot_manager::SnapshotManagerTrait,
    storage_db::{
        DeltaDbManagerTrait, KeyValueDbIterableTrait, SnapshotDbManagerTrait,
        SnapshotInfo, SnapshotKeptToProvideSyncStatus,
    },
    utils::guarded_value::GuardedValue,
    DeltaMpt, DeltaMptIdGen, DeltaMptIterator, KeyValueDbTrait,
    OpenDeltaDbLru, ProvideExtraSnapshotSyncConfig,
    StateIndex, StateRootWithAuxInfo, StorageConfiguration,
};
use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};
use mazze_internal_common::{
    consensus_api::StateMaintenanceTrait, StateAvailabilityBoundary,
};
use parking_lot::{Mutex, RwLock, RwLockReadGuard};
use primitives::{EpochId, MerkleHash, MERKLE_NULL_NODE, NULL_EPOCH};
use rlp::{Decodable, DecoderError, Encodable, Rlp};
use lazy_static::lazy_static;
use metrics::{register_meter_with_group, Meter};
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    fs,
    sync::{
        atomic::AtomicBool,
        mpsc::{channel, Sender},
        Arc, Weak,
    },
    thread::{self, JoinHandle},
};

/// Phase E — Atomic backing for the `snapshot.in_flight` gauge. The
/// `Gauge` trait only exposes `update`, so we keep the source of truth
/// here and push the value into the gauge on every change.
static SNAPSHOT_IN_FLIGHT_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

lazy_static! {
    /// Bumped when a background snapshot-creation thread observes
    /// `cancel_requested` (set by `maintain_snapshots_main_chain_confirmed`
    /// when the snapshot's target is determined to be on a
    /// non-canonical fork) and self-aborts before registration. See
    /// Phase B.1 in `docs/checkpoint-snapshot-lifecycle.md`.
    static ref SNAPSHOT_CANCELLED_TOTAL: Arc<dyn Meter> =
        register_meter_with_group("snapshot", "cancelled_total");

    /// Bumped when `register_new_snapshot` accepts a new snapshot
    /// after passing all sanity checks (merkle_root, parent linkage).
    static ref SNAPSHOT_REGISTERED_TOTAL: Arc<dyn Meter> =
        register_meter_with_group("snapshot", "registered_total");

    /// Bumped when `register_new_snapshot` rejects a snapshot for a
    /// zero merkle_root on a non-NULL_EPOCH parent. Indicates either a
    /// default-init bug or merge corruption. See Phase C.1.
    static ref SNAPSHOT_INVALID_ROOT_TOTAL: Arc<dyn Meter> =
        register_meter_with_group("snapshot", "invalid_root_at_registration_total");

    /// Bumped when `register_new_snapshot` rejects a snapshot because
    /// its parent snapshot has been pruned or never existed. See
    /// Phase C.4.
    static ref SNAPSHOT_ORPHAN_REJECTED_TOTAL: Arc<dyn Meter> =
        register_meter_with_group("snapshot", "orphan_rejected_at_registration_total");

    /// Phase E — Bumped when the background snapshot thread returns
    /// `Err` (anywhere in `new_snapshot_by_merging` — disk full, MPT
    /// corruption, lock contention, etc.). Distinct from
    /// `cancelled_total` (B.1, voluntary fork-cancellation) and from
    /// `invalid_root_at_registration_total` / `orphan_rejected_at_registration_total`
    /// (C.1 / C.4, rejected at registration after a successful merge).
    static ref SNAPSHOT_FAILED_MERGE_TOTAL: Arc<dyn Meter> =
        register_meter_with_group("snapshot", "failed_total.merge_error");

    /// Phase E — Bumped on the catch-all `Err` return from
    /// `register_new_snapshot` that isn't the merkle-root / orphan
    /// reject. Captures DB-layer write failures during registration.
    static ref SNAPSHOT_FAILED_REGISTER_TOTAL: Arc<dyn Meter> =
        register_meter_with_group("snapshot", "failed_total.register_error");

    /// Phase E — Bumped on every `destroy_snapshot` call from
    /// `StorageManager` (retention pruning + non-canonical-fork
    /// cleanup). Doesn't fire from `register_new_snapshot`'s reject
    /// paths because those never wrote anything to disk.
    static ref SNAPSHOT_PRUNED_TOTAL: Arc<dyn Meter> =
        register_meter_with_group("snapshot", "pruned_total");

    /// Phase E — Cumulative milliseconds spent inside the background
    /// snapshot thread between spawn and completion (whether the
    /// merge committed, was cancelled, or errored). Divide by
    /// `cancelled + registered + failed_merge` for a mean per-snapshot
    /// cost. Cheap to maintain — one extra `Instant::elapsed` per
    /// snapshot.
    static ref SNAPSHOT_CREATION_DURATION_MS_TOTAL: Arc<dyn Meter> =
        register_meter_with_group("snapshot", "creation_duration_ms_total");

    /// Phase E — Gauge of the current number of in-flight snapshot
    /// background threads. Sustained `> 1` over multiple minutes
    /// indicates a stuck merge or a stuck joiner; should normally be
    /// 0 with brief spikes to 1 around era boundaries.
    static ref SNAPSHOT_IN_FLIGHT_GAUGE: Arc<dyn metrics::Gauge<usize>> =
        metrics::GaugeUsize::register_with_group("snapshot", "in_flight");

    /// Phase 5c — Bytes actually occupied on disk by the dedicated
    /// snapshot MDBX env (`storage_db/mdbx_snapshot/`). Sampled from
    /// `KvdbMdbxStats::bytes_used` on the col-0 handle at each
    /// `log_usage` tick. Snapshot data is uncompressed and the env's
    /// map has a hard `max_mb` ceiling — this gauge is the earliest
    /// warning of a full-map halt.
    static ref SNAPSHOT_MDBX_BYTES_USED: Arc<dyn metrics::Gauge<usize>> =
        metrics::GaugeUsize::register_with_group("mdbx_snapshot", "bytes_used");

    /// Phase 5c — `bytes_used` as a percentage of the env's
    /// configured `max_mb`. This is the operator-facing number: the
    /// alert threshold at 80% is enforced by a `warn!` in
    /// `sample_snapshot_mdbx_capacity`. Multiplied by 100 (i.e. an
    /// integer 0..=10_000 encodes 0.00-100.00%) so we don't lose
    /// resolution against a `Gauge<usize>`.
    static ref SNAPSHOT_MDBX_MAP_PCT_USED_X100: Arc<dyn metrics::Gauge<usize>> =
        metrics::GaugeUsize::register_with_group(
            "mdbx_snapshot",
            "map_pct_used_x100",
        );
}

/// The operator-alert threshold at which `sample_snapshot_mdbx_capacity`
/// emits a warn-level log. Kept below 100 so operators have runway to
/// raise `snapshot_mdbx_max_mb` in hydra.toml + restart before the map
/// fills. See design doc §2.3.1.
const SNAPSHOT_MDBX_CAPACITY_WARN_PCT: u64 = 80;

/// Phase E — Helper that adjusts the in-flight count and republishes
/// it into the gauge. Called on bg-thread spawn (delta=+1) and on
/// completion (delta=-1).
fn snapshot_in_flight_adjust(delta: i64) {
    let new_value = if delta >= 0 {
        SNAPSHOT_IN_FLIGHT_COUNT
            .fetch_add(delta as usize, std::sync::atomic::Ordering::SeqCst)
            .saturating_add(delta as usize)
    } else {
        let dec = (-delta) as usize;
        let prev = SNAPSHOT_IN_FLIGHT_COUNT
            .fetch_sub(dec, std::sync::atomic::Ordering::SeqCst);
        prev.saturating_sub(dec)
    };
    SNAPSHOT_IN_FLIGHT_GAUGE.update(new_value);
}

/// Pure integer arithmetic for the `mdbx_snapshot.map_pct_used_x100`
/// gauge — returns the occupancy as basis-points × 100 (i.e. an
/// integer 0..=10_000 encodes 0.00-100.00%). Returns `None` when
/// `max_bytes == 0` so the caller can leave the gauge untouched
/// instead of publishing a bogus zero. Multiplies through `u128` so
/// `bytes_used × 10_000` cannot overflow at any plausible map size.
fn snapshot_mdbx_pct_x100(bytes_used: u64, max_bytes: u64) -> Option<u64> {
    if max_bytes == 0 {
        return None;
    }
    let pct_x100 =
        (bytes_used as u128).saturating_mul(10_000) / max_bytes as u128;
    Some(pct_x100 as u64)
}

#[cfg(test)]
mod snapshot_mdbx_gauge_tests {
    use super::snapshot_mdbx_pct_x100;

    #[test]
    fn zero_bytes_used() {
        assert_eq!(snapshot_mdbx_pct_x100(0, 1024 * 1024).unwrap(), 0);
    }

    #[test]
    fn half_full() {
        // 512 MB out of 1024 MB → 50.00% → 5000.
        let mb: u64 = 1024 * 1024;
        assert_eq!(
            snapshot_mdbx_pct_x100(512 * mb, 1024 * mb).unwrap(),
            5000
        );
    }

    #[test]
    fn full_and_over_full() {
        let m = 1024;
        // Exactly full → 10_000 (100.00%).
        assert_eq!(snapshot_mdbx_pct_x100(m, m).unwrap(), 10_000);
        // Over-full (page rounding can push bytes_used past max) —
        // must not overflow; integer division just returns > 10_000.
        assert_eq!(snapshot_mdbx_pct_x100(m * 2, m).unwrap(), 20_000);
    }

    #[test]
    fn zero_max_returns_none() {
        // Sentinel path for the "disabled" case; caller uses this to
        // skip the gauge update.
        assert!(snapshot_mdbx_pct_x100(1_000_000, 0).is_none());
    }

    #[test]
    fn huge_bytes_used_does_not_overflow() {
        // 1 EB used, 32 GB ceiling — the intermediate multiply must
        // stay inside u128.
        let one_eb = 1u64 << 60;
        let max = 32u64 * 1024 * 1024 * 1024;
        let pct = snapshot_mdbx_pct_x100(one_eb, max).unwrap();
        assert!(pct > 100_000_000);
    }
}
