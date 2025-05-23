// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

pub type DeltaDbManager = DeltaDbManagerRocksdb;
pub type SnapshotDbManager = SnapshotDbManagerSqlite;
pub type SnapshotDb = <SnapshotDbManager as SnapshotDbManagerTrait>::SnapshotDb;

pub struct StateTrees {
    pub snapshot_db: SnapshotDb,
    pub snapshot_epoch_id: EpochId,
    pub snapshot_merkle_root: MerkleHash,
    /// None means that the intermediate_trie is empty, or in a special
    /// situation that we use the snapshot at intermediate epoch directly,
    /// so we don't need to look up intermediate trie.
    pub maybe_intermediate_trie: Option<Arc<DeltaMpt>>,
    pub intermediate_trie_root: Option<NodeRefDeltaMpt>,
    pub intermediate_trie_root_merkle: MerkleHash,
    /// A None value indicate the special case when snapshot_db is actually the
    /// snapshot_db from the intermediate_epoch_id.
    pub maybe_intermediate_trie_key_padding: Option<DeltaMptKeyPadding>,
    /// Delta trie can't be none since we may commit into it.
    pub delta_trie: Arc<DeltaMpt>,
    pub delta_trie_root: Option<NodeRefDeltaMpt>,
    pub delta_trie_key_padding: DeltaMptKeyPadding,
    /// Information for making new snapshot when necessary.
    pub maybe_delta_trie_height: Option<u32>,
    pub maybe_height: Option<u64>,
    pub intermediate_epoch_id: EpochId,

    // TODO: this field is added only for the hack to get main chain from a
    // TODO: snapshot to its parent snapshot.
    pub parent_epoch_id: EpochId,
}

#[derive(MallocSizeOfDerive)]
pub struct StateManager {
    storage_manager: Arc<StorageManager>,
    single_mpt_storage_manager: Option<Arc<SingleMptStorageManager>>,
    pub number_committed_nodes: AtomicUsize,
}

impl Drop for StateManager {
    fn drop(&mut self) {
        self.storage_manager.graceful_shutdown();
    }
}

impl StateManager {
    pub fn new(conf: StorageConfiguration) -> Result<Self> {
        debug!("Storage conf {:?}", conf);
        // Make sure sqlite temp directory is using the data disk instead of the
        // system disk.
        std::env::set_var("SQLITE_TMPDIR", conf.path_snapshot_dir.clone());

        let single_mpt_storage_manager = if conf.enable_single_mpt_storage {
            Some(SingleMptStorageManager::new_arc(
                conf.path_storage_dir.join("single_mpt"),
                conf.single_mpt_space,
                conf.single_mpt_cache_start_size,
                conf.single_mpt_cache_size,
                conf.single_mpt_slab_idle_size,
            ))
        } else {
            None
        };

        let storage_manager = StorageManager::new_arc(conf)?;
        Ok(Self {
            storage_manager,
            single_mpt_storage_manager,
            number_committed_nodes: Default::default(),
        })
    }

    pub fn log_usage(&self) {
        self.storage_manager.log_usage();
        debug!(
            "number of nodes committed to db {}",
            self.number_committed_nodes.load(Ordering::Relaxed),
        );
    }

    pub fn get_storage_manager(&self) -> &StorageManager {
        &*self.storage_manager
    }

    pub fn get_storage_manager_arc(&self) -> &Arc<StorageManager> {
        &self.storage_manager
    }

    /// delta_mpt_key_padding is required. When None is passed,
    /// it's calculated for the state_trees.
    #[inline]
    pub fn get_state_trees_internal(
        snapshot_db: SnapshotDb, snapshot_epoch_id: &EpochId,
        snapshot_merkle_root: MerkleHash,
        maybe_intermediate_trie: Option<Arc<DeltaMpt>>,
        maybe_intermediate_trie_key_padding: Option<&DeltaMptKeyPadding>,
        intermediate_epoch_id: &EpochId,
        intermediate_trie_root_merkle: MerkleHash, delta_mpt: Arc<DeltaMpt>,
        maybe_delta_mpt_key_padding: Option<&DeltaMptKeyPadding>,
        epoch_id: &EpochId, delta_root: Option<NodeRefDeltaMpt>,
        maybe_height: Option<u64>, maybe_delta_trie_height: Option<u32>,
    ) -> Result<Option<StateTrees>> {
        let intermediate_trie_root = match &maybe_intermediate_trie {
            None => None,
            Some(mpt) => {
                match mpt.get_root_node_ref_by_epoch(intermediate_epoch_id)? {
                    None => {
                        warn!(
                            "get_state_trees_internal, intermediate_mpt root not found \
                             for epoch {:?}.",
                            intermediate_epoch_id,
                        );
                        return Ok(None);
                    }
                    Some(root) => root,
                }
            }
        };

        let delta_trie_key_padding = match maybe_delta_mpt_key_padding {
            Some(x) => x.clone(),
            None => {
                // TODO: maybe we can move the calculation to a central place
                // and cache the result?
                StorageKeyWithSpace::delta_mpt_padding(
                    &snapshot_merkle_root,
                    &intermediate_trie_root_merkle,
                )
            }
        };

        Ok(Some(StateTrees {
            snapshot_db,
            snapshot_merkle_root,
            snapshot_epoch_id: *snapshot_epoch_id,
            maybe_intermediate_trie,
            intermediate_trie_root,
            intermediate_trie_root_merkle,
            maybe_intermediate_trie_key_padding:
                maybe_intermediate_trie_key_padding.cloned(),
            delta_trie: delta_mpt,
            delta_trie_root: delta_root,
            delta_trie_key_padding,
            maybe_delta_trie_height,
            maybe_height,
            intermediate_epoch_id: intermediate_epoch_id.clone(),
            parent_epoch_id: epoch_id.clone(),
        }))
    }

    pub fn get_state_trees(
        &self, state_index: &StateIndex, try_open: bool,
        open_mpt_snapshot: bool,
    ) -> Result<Option<StateTrees>> {
        let maybe_intermediate_mpt;
        let maybe_intermediate_mpt_key_padding;
        let delta_mpt;
        let snapshot;

        match self.storage_manager.wait_for_snapshot(
            &state_index.snapshot_epoch_id,
            try_open,
            open_mpt_snapshot,
        )? {
            None => {
                // This is the special scenario when the snapshot isn't
                // available but the snapshot at the intermediate epoch exists.
                if let Some(guarded_snapshot) =
                    self.storage_manager.wait_for_snapshot(
                        &state_index.intermediate_epoch_id,
                        try_open,
                        open_mpt_snapshot,
                    )?
                {
                    snapshot = guarded_snapshot;
                    maybe_intermediate_mpt = None;
                    maybe_intermediate_mpt_key_padding = None;
                    delta_mpt = match self
                        .storage_manager
                        .get_intermediate_mpt(
                            &state_index.intermediate_epoch_id,
                        )? {
                        None => {
                            warn!(
                                    "get_state_trees, special case, \
                                    intermediate_mpt not found for epoch {:?}. StateIndex: {:?}.",
                                    state_index.intermediate_epoch_id,
                                    state_index,
                                );
                            return Ok(None);
                        }
                        Some(delta_mpt) => delta_mpt,
                    };
                } else {
                    warn!(
                        "get_state_trees, special case, \
                         snapshot not found for epoch {:?}. StateIndex: {:?}.",
                        state_index.intermediate_epoch_id, state_index,
                    );
                    return Ok(None);
                }
            }
            Some(guarded_snapshot) => {
                snapshot = guarded_snapshot;
                maybe_intermediate_mpt_key_padding =
                    state_index.maybe_intermediate_mpt_key_padding.as_ref();
                maybe_intermediate_mpt = if maybe_intermediate_mpt_key_padding
                    .is_some()
                {
                    self.storage_manager
                        .get_intermediate_mpt(&state_index.snapshot_epoch_id)?
                } else {
                    None
                };
                delta_mpt = self
                    .storage_manager
                    .get_delta_mpt(&state_index.snapshot_epoch_id)?;
            }
        }

        let delta_root = match delta_mpt
            .get_root_node_ref_by_epoch(&state_index.epoch_id)?
        {
            None => {
                debug!(
                    "get_state_trees, \
                    delta_root not found for epoch {:?}. mpt_id {}, StateIndex: {:?}.",
                    state_index.epoch_id, delta_mpt.get_mpt_id(), state_index,
                );
                return Ok(None);
            }
            Some(root) => root,
        };

        Self::get_state_trees_internal(
            snapshot.into().1,
            &state_index.snapshot_epoch_id,
            state_index.snapshot_merkle_root,
            maybe_intermediate_mpt,
            maybe_intermediate_mpt_key_padding,
            &state_index.intermediate_epoch_id,
            state_index.intermediate_trie_root_merkle,
            delta_mpt,
            Some(&state_index.delta_mpt_key_padding),
            &state_index.epoch_id,
            delta_root,
            state_index.maybe_height,
            state_index.maybe_delta_trie_height,
        )
    }

    pub fn get_state_trees_for_next_epoch(
        &self, parent_state_index: &StateIndex, try_open: bool,
        open_mpt_snapshot: bool,
    ) -> Result<Option<StateTrees>> {
        let maybe_height = parent_state_index.maybe_height.map(|x| x + 1);

        let snapshot;
        let snapshot_epoch_id;
        let snapshot_merkle_root;
        let maybe_delta_trie_height;
        let maybe_intermediate_mpt;
        let maybe_intermediate_mpt_key_padding;
        let intermediate_trie_root_merkle;
        let delta_mpt;
        let maybe_delta_mpt_key_padding;
        let intermediate_epoch_id;
        let new_delta_root;

        if parent_state_index
            .maybe_delta_trie_height
            .unwrap_or_default()
            == self.storage_manager.get_snapshot_epoch_count()
        {
            // Should shift to a new snapshot
            // When the delta_height is set to None (e.g. in tests), we
            // assume that the snapshot shift check is
            // disabled.

            snapshot_epoch_id = &parent_state_index.intermediate_epoch_id;
            intermediate_epoch_id = &parent_state_index.epoch_id;
            match self.storage_manager.wait_for_snapshot(
                snapshot_epoch_id,
                try_open,
                open_mpt_snapshot,
            )? {
                None => {
                    // This is the special scenario when the snapshot isn't
                    // available but the snapshot at the intermediate epoch
                    // exists.
                    //
                    // At the synced snapshot, the intermediate_epoch_id is
                    // its parent snapshot. We need to shift again.

                    // There is no snapshot_info for the parent snapshot,
                    // how can we find out the snapshot_merkle_root?
                    // See validate_blame_states().
                    match self.storage_manager.wait_for_snapshot(
                        &intermediate_epoch_id,
                        try_open,
                        open_mpt_snapshot,
                    )? {
                        None => {
                            warn!(
                                "get_state_trees_for_next_epoch, shift snapshot, special case, \
                                snapshot not found for snapshot {:?}. StateIndex: {:?}.",
                                parent_state_index.epoch_id,
                                parent_state_index,
                            );
                            return Ok(None);
                        }
                        Some(guarded_snapshot) => {
                            let (guard, _snapshot) = guarded_snapshot.into();
                            snapshot_merkle_root =
                                match StorageManager::find_merkle_root(
                                    &guard,
                                    snapshot_epoch_id,
                                ) {
                                    None => {
                                        warn!(
                                            "get_state_trees_for_next_epoch, shift snapshot, special case, \
                                            snapshot merkel root not found for snapshot {:?}. StateIndex: {:?}.",
                                            snapshot_epoch_id,
                                            parent_state_index,
                                        );
                                        return Ok(None);
                                    }
                                    Some(merkle_root) => merkle_root,
                                };

                            snapshot = GuardedValue::new(guard, _snapshot);
                        }
                    }
                    maybe_intermediate_mpt = None;
                    maybe_intermediate_mpt_key_padding = None;
                    match self
                        .storage_manager
                        .intermediate_trie_root_merkle
                        .write()
                        .take()
                    {
                        Some(v) => {
                            intermediate_trie_root_merkle = v;
                        }
                        _ => {
                            warn!(
                                "get_state_trees_for_next_epoch, shift snapshot, special case, \
                                intermediate_trie_root_merkle not found for snapshot {:?}. StateIndex: {:?}.",
                                snapshot_epoch_id,
                                parent_state_index,
                            );
                            return Ok(None);
                        }
                    }

                    debug!("get_state_trees_for_next_epoch, snapshot_merkle_root {:?}, intermediate_trie_root_merkle {:?}", snapshot_merkle_root, intermediate_trie_root_merkle);

                    match self
                        .storage_manager
                        .get_intermediate_mpt(&parent_state_index.epoch_id)?
                    {
                        None => {
                            warn!(
                                "get_state_trees_for_next_epoch, shift snapshot, special case, \
                                intermediate_mpt not found for snapshot {:?}. StateIndex: {:?}.",
                                parent_state_index.epoch_id,
                                parent_state_index,
                            );
                            return Ok(None);
                        }
                        Some(mpt) => delta_mpt = mpt,
                    }
                }
                Some(guarded_snapshot) => {
                    let (guard, _snapshot) = guarded_snapshot.into();
                    snapshot_merkle_root =
                        match StorageManager::find_merkle_root(
                            &guard,
                            snapshot_epoch_id,
                        ) {
                            None => {
                                warn!(
                                "get_state_trees_for_next_epoch, shift snapshot, normal case, \
                                snapshot info not found for snapshot {:?}. StateIndex: {:?}.",
                                snapshot_epoch_id,
                                parent_state_index,
                            );
                                return Ok(None);
                            }
                            Some(merkle_root) => merkle_root,
                        };
                    let guarded_snapshot = GuardedValue::new(guard, _snapshot);
                    let temp_maybe_intermediate_mpt = self
                        .storage_manager
                        .get_intermediate_mpt(snapshot_epoch_id)?;
                    match temp_maybe_intermediate_mpt {
                        None => {
                            snapshot = guarded_snapshot;
                            maybe_intermediate_mpt_key_padding =
                                Some(&parent_state_index.delta_mpt_key_padding);
                            delta_mpt = self
                                .storage_manager
                                .get_delta_mpt(&snapshot_epoch_id)?;
                            intermediate_trie_root_merkle = MERKLE_NULL_NODE;
                            maybe_intermediate_mpt = None;
                        }
                        Some(mpt) => match mpt.get_merkle_root_by_epoch_id(
                            &parent_state_index.epoch_id,
                        )? {
                            Some(merkle_root) => {
                                snapshot = guarded_snapshot;
                                maybe_intermediate_mpt_key_padding = Some(
                                    &parent_state_index.delta_mpt_key_padding,
                                );
                                delta_mpt = self
                                    .storage_manager
                                    .get_delta_mpt(&snapshot_epoch_id)?;
                                intermediate_trie_root_merkle = merkle_root;
                                maybe_intermediate_mpt = Some(mpt);
                            }
                            None => {
                                warn!(
                                        "get_state_trees_for_next_epoch, shift snapshot, normal case, \
                                        intermediate_trie_root not found for epoch {:?}. StateIndex: {:?}.",
                                        parent_state_index.epoch_id,
                                        parent_state_index,
                                    );

                                // Check if we should progress with a synced
                                // state.
                                // This is a quick fix for
                                // https://github.com/s94130586/mazze-rust/issues/1543.
                                // FIXME: We should remove all the hacks about
                                // state sync.
                                match self.storage_manager.wait_for_snapshot(
                                    &parent_state_index.epoch_id,
                                    try_open,
                                    open_mpt_snapshot,
                                )? {
                                    None => {
                                        warn!(
                                            "get_state_trees_for_next_epoch, shift snapshot, special case, \
                                snapshot not found for snapshot {:?}. StateIndex: {:?}.",
                                            parent_state_index.epoch_id,
                                            parent_state_index,
                                        );
                                        return Ok(None);
                                    }
                                    Some(guarded_synced_snapshot) => {
                                        snapshot = guarded_synced_snapshot;
                                    }
                                }
                                maybe_intermediate_mpt = None;
                                maybe_intermediate_mpt_key_padding = None;
                                match self
                                    .storage_manager
                                    .intermediate_trie_root_merkle
                                    .write()
                                    .take()
                                {
                                    Some(v) => {
                                        intermediate_trie_root_merkle = v;
                                    }
                                    _ => {
                                        warn!("get_state_trees_for_next_epoch, shift snapshot, special case, \
                                        intermediate_trie_root_merkle not found for snapshot {:?}. StateIndex: {:?}.", snapshot_epoch_id, parent_state_index,);
                                        return Ok(None);
                                    }
                                }

                                match self
                                    .storage_manager
                                    .get_intermediate_mpt(
                                        &parent_state_index.epoch_id,
                                    )? {
                                    None => {
                                        warn!(
                                            "get_state_trees_for_next_epoch, shift snapshot, special case, \
                                intermediate_mpt not found for snapshot {:?}. StateIndex: {:?}.",
                                            parent_state_index.epoch_id,
                                            parent_state_index,
                                        );
                                        return Ok(None);
                                    }
                                    Some(mpt) => delta_mpt = mpt,
                                }
                            }
                        },
                    };
                }
            };
            maybe_delta_mpt_key_padding = None;
            maybe_delta_trie_height = Some(1);
            new_delta_root = true;
        } else {
            snapshot_epoch_id = &parent_state_index.snapshot_epoch_id;
            snapshot_merkle_root = parent_state_index.snapshot_merkle_root;
            intermediate_epoch_id = &parent_state_index.intermediate_epoch_id;
            intermediate_trie_root_merkle =
                parent_state_index.intermediate_trie_root_merkle;
            match self.storage_manager.wait_for_snapshot(
                snapshot_epoch_id,
                try_open,
                open_mpt_snapshot,
            )? {
                None => {
                    // This is the special scenario when the snapshot isn't
                    // available but the snapshot at the intermediate epoch
                    // exists.
                    if let Some(guarded_snapshot) =
                        self.storage_manager.wait_for_snapshot(
                            &intermediate_epoch_id,
                            try_open,
                            open_mpt_snapshot,
                        )?
                    {
                        snapshot = guarded_snapshot;
                        maybe_intermediate_mpt = None;
                        maybe_intermediate_mpt_key_padding = None;
                        delta_mpt = match self
                            .storage_manager
                            .get_intermediate_mpt(intermediate_epoch_id)?
                        {
                            None => {
                                return {
                                    warn!(
                                    "get_state_trees_for_next_epoch, special case, \
                                    intermediate_mpt not found for epoch {:?}. StateIndex: {:?}.",
                                    intermediate_epoch_id,
                                    parent_state_index,
                                );
                                    Ok(None)
                                }
                            }
                            Some(delta_mpt) => delta_mpt,
                        };
                    } else {
                        warn!(
                            "get_state_trees_for_next_epoch, special case, \
                            snapshot not found for epoch {:?}. StateIndex: {:?}.",
                            intermediate_epoch_id,
                            parent_state_index,
                        );
                        return Ok(None);
                    }
                }
                Some(guarded_snapshot) => {
                    snapshot = guarded_snapshot;
                    maybe_intermediate_mpt_key_padding = parent_state_index
                        .maybe_intermediate_mpt_key_padding
                        .as_ref();
                    maybe_intermediate_mpt =
                        if maybe_intermediate_mpt_key_padding.is_some() {
                            self.storage_manager
                                .get_intermediate_mpt(snapshot_epoch_id)?
                        } else {
                            None
                        };
                    delta_mpt = self
                        .storage_manager
                        .get_delta_mpt(snapshot_epoch_id)?;
                }
            };
            maybe_delta_trie_height =
                parent_state_index.maybe_delta_trie_height.map(|x| x + 1);
            maybe_delta_mpt_key_padding =
                Some(&parent_state_index.delta_mpt_key_padding);
            new_delta_root = false;
        };

        let delta_root = if new_delta_root {
            None
        } else {
            match delta_mpt
                .get_root_node_ref_by_epoch(&parent_state_index.epoch_id)?
            {
                None => {
                    warn!(
                        "get_state_trees_for_next_epoch, not shifting, \
                         delta_root not found for epoch {:?}. mpt_id {}, StateIndex: {:?}.",
                        parent_state_index.epoch_id,
                        delta_mpt.get_mpt_id(), parent_state_index
                    );
                    return Ok(None);
                }
                Some(root_node) => root_node,
            }
        };
        Self::get_state_trees_internal(
            snapshot.into().1,
            snapshot_epoch_id,
            snapshot_merkle_root,
            maybe_intermediate_mpt,
            maybe_intermediate_mpt_key_padding,
            intermediate_epoch_id,
            intermediate_trie_root_merkle,
            delta_mpt,
            maybe_delta_mpt_key_padding,
            &parent_state_index.epoch_id,
            delta_root,
            maybe_height,
            maybe_delta_trie_height,
        )
    }

    /// Check if we can make a new snapshot, and if so, make it in background.
    pub fn check_make_snapshot(
        &self, maybe_intermediate_trie: Option<Arc<DeltaMpt>>,
        intermediate_trie_root: Option<NodeRefDeltaMpt>,
        intermediate_epoch_id: &EpochId, new_height: u64,
        recover_mpt_during_construct_main_state: bool,
    ) -> Result<()> {
        StorageManager::check_make_register_snapshot_background(
            self.storage_manager.clone(),
            intermediate_epoch_id.clone(),
            new_height,
            maybe_intermediate_trie.map(|intermediate_trie| DeltaMptIterator {
                mpt: intermediate_trie,
                maybe_root_node: intermediate_trie_root,
            }),
            recover_mpt_during_construct_main_state,
        )
    }

    pub fn get_state_no_commit_inner(
        self: &Arc<Self>, state_index: StateIndex, try_open: bool,
        open_mpt_snapshot: bool,
    ) -> Result<Option<State>> {
        let maybe_state_trees =
            self.get_state_trees(&state_index, try_open, open_mpt_snapshot)?;
        match maybe_state_trees {
            None => Ok(None),
            Some(state_trees) => {
                Ok(Some(State::new(self.clone(), state_trees, false)))
            }
        }
    }

    fn get_state_for_genesis_write_inner(self: &Arc<Self>) -> State {
        State::new(
            self.clone(),
            StateTrees {
                snapshot_db: self
                    .storage_manager
                    .wait_for_snapshot(
                        &NULL_EPOCH,
                        /* try_open = */ false,
                        true,
                    )
                    .unwrap()
                    .unwrap()
                    .into()
                    .1,
                snapshot_epoch_id: NULL_EPOCH,
                snapshot_merkle_root: MERKLE_NULL_NODE,
                maybe_intermediate_trie: None,
                intermediate_trie_root: None,
                intermediate_trie_root_merkle: MERKLE_NULL_NODE,
                maybe_intermediate_trie_key_padding: None,
                delta_trie: self
                    .storage_manager
                    .get_delta_mpt(&NULL_EPOCH)
                    .unwrap(),
                delta_trie_root: None,
                delta_trie_key_padding: GENESIS_DELTA_MPT_KEY_PADDING.clone(),
                maybe_delta_trie_height: Some(1),
                maybe_height: Some(1),
                intermediate_epoch_id: NULL_EPOCH,
                parent_epoch_id: NULL_EPOCH,
            },
            false,
        )
    }

    // Currently we use epoch number to decide whether or not to
    // start a new delta trie. The value of parent_epoch_id is only
    // known after the computation is done.
    //
    // If we use delta trie size upper bound to decide whether or not
    // to start a new delta trie, then the computation about whether
    // or not start a new delta trie, can only be done at the time
    // of committing. In this scenario, the execution engine should
    // first get the state assuming that the delta trie won't change,
    // then check if committing fails due to over size, and if so,
    // start a new delta trie and re-apply the change.
    //
    // Due to the complexity of the latter approach, we stay with the
    // simple approach.
    pub fn get_state_for_next_epoch_inner(
        self: &Arc<Self>, parent_epoch_id: StateIndex, open_mpt_snapshot: bool,
        recover_mpt_during_construct_main_state: bool,
    ) -> Result<Option<State>> {
        let maybe_state_trees = self.get_state_trees_for_next_epoch(
            &parent_epoch_id,
            /* try_open = */ false,
            open_mpt_snapshot,
        )?;
        match maybe_state_trees {
            None => Ok(None),
            Some(state_trees) => Ok(Some(State::new(
                self.clone(),
                state_trees,
                recover_mpt_during_construct_main_state,
            ))),
        }
    }

    pub fn notify_genesis_hash(&self, genesis_hash: EpochId) {
        if let Some(single_mpt_manager) = &self.single_mpt_storage_manager {
            *single_mpt_manager.genesis_hash.lock() = genesis_hash;
        }
    }

    pub fn config(&self) -> &StorageConfiguration {
        &self.storage_manager.storage_conf
    }
}

impl StateManagerTrait for StateManager {
    fn get_state_no_commit(
        self: &Arc<Self>, state_index: StateIndex, try_open: bool,
        space: Option<Space>,
    ) -> Result<Option<Box<dyn StateTrait>>> {
        let maybe_state_trees =
            self.get_state_trees(&state_index, try_open, false);
        // If there is an error, we will continue to search for an available
        // single_mpt.
        let maybe_state_err = match maybe_state_trees {
            Ok(Some(state_trees)) => {
                return Ok(Some(Box::new(State::new(
                    self.clone(),
                    state_trees,
                    false,
                ))));
            }
            Err(e) => Err(e),
            Ok(None) => Ok(None),
        };
        if self.single_mpt_storage_manager.is_none() {
            return maybe_state_err;
        }
        let single_mpt_storage_manager =
            self.single_mpt_storage_manager.as_ref().unwrap();
        if !single_mpt_storage_manager.contains_space(&space) {
            return maybe_state_err;
        }
        debug!(
            "read state from single mpt state: epoch={}",
            state_index.epoch_id
        );
        let single_mpt_state = single_mpt_storage_manager
            .get_state_by_epoch(state_index.epoch_id)?;
        if single_mpt_state.is_none() {
            warn!("single mpt state missing: epoch={:?}", state_index.epoch_id);
            return maybe_state_err;
        } else {
            Ok(Some(Box::new(single_mpt_state.unwrap())))
        }
    }

    fn get_state_for_genesis_write(self: &Arc<Self>) -> Box<dyn StateTrait> {
        let state = self.get_state_for_genesis_write_inner();
        if self.single_mpt_storage_manager.is_none() {
            return Box::new(state);
        }
        let single_mpt_storage_manager =
            self.single_mpt_storage_manager.as_ref().unwrap();
        let single_mpt_state = single_mpt_storage_manager
            .get_state_for_genesis()
            .expect("single_mpt genesis initialize error");
        Box::new(ReplicatedState::new(
            state,
            single_mpt_state,
            single_mpt_storage_manager.get_state_filter(),
        ))
    }

    // Currently we use epoch number to decide whether or not to
    // start a new delta trie. The value of parent_epoch_id is only
    // known after the computation is done.
    //
    // If we use delta trie size upper bound to decide whether or not
    // to start a new delta trie, then the computation about whether
    // or not start a new delta trie, can only be done at the time
    // of committing. In this scenario, the execution engine should
    // first get the state assuming that the delta trie won't change,
    // then check if committing fails due to over size, and if so,
    // start a new delta trie and re-apply the change.
    //
    // Due to the complexity of the latter approach, we stay with the
    // simple approach.
    fn get_state_for_next_epoch(
        self: &Arc<Self>, parent_epoch_id: StateIndex,
        recover_mpt_during_construct_main_state: bool,
    ) -> Result<Option<Box<dyn StateTrait>>> {
        let parent_epoch = parent_epoch_id.epoch_id;
        let state = self.get_state_for_next_epoch_inner(
            parent_epoch_id,
            false,
            recover_mpt_during_construct_main_state,
        )?;
        if state.is_none() {
            return Ok(None);
        }
        if self.single_mpt_storage_manager.is_none() {
            return Ok(Some(Box::new(state.unwrap())));
        }
        let single_mpt_storage_manager =
            self.single_mpt_storage_manager.as_ref().unwrap();
        let single_mpt_state =
            single_mpt_storage_manager.get_state_by_epoch(parent_epoch)?;
        if single_mpt_state.is_none() {
            error!("get_state_for_next_epoch: single_mpt_state is required but is not found!");
            return Ok(None);
        }
        Ok(Some(Box::new(ReplicatedState::new(
            state.unwrap(),
            single_mpt_state.unwrap(),
            single_mpt_storage_manager.get_state_filter(),
        ))))
    }
}

use crate::{
    impls::{
        delta_mpt::*,
        errors::*,
        replicated_state::ReplicatedState,
        storage_db::{
            delta_db_manager_rocksdb::DeltaDbManagerRocksdb,
            snapshot_db_manager_sqlite::SnapshotDbManagerSqlite,
        },
        storage_manager::{
            single_mpt_storage_manager::SingleMptStorageManager,
            storage_manager::StorageManager,
        },
    },
    state::*,
    state_manager::*,
    storage_db::*,
    utils::guarded_value::GuardedValue,
    StorageConfiguration,
};
use malloc_size_of_derive::MallocSizeOf as MallocSizeOfDerive;
use mazze_types::Space;
use primitives::{
    DeltaMptKeyPadding, EpochId, MerkleHash, StorageKeyWithSpace,
    GENESIS_DELTA_MPT_KEY_PADDING, MERKLE_NULL_NODE, NULL_EPOCH,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
