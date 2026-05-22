// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::{
    sync::{
        message::DynamicCapability,
        state::{SnapshotChunkSync, Status},
        synchronization_protocol_handler::SynchronizationProtocolHandler,
        synchronization_state::SynchronizationState,
        SharedSynchronizationGraph,
    },
    ConsensusGraph,
};
use mazze_internal_common::{
    EpochExecutionCommitment, StateAvailabilityBoundary,
};
use mazze_parameters::sync::CATCH_UP_EPOCH_LAG_THRESHOLD;
use mazze_types::H256;
use metrics::{Counter, CounterUsize, Gauge, GaugeUsize};
use network::NetworkContext;
use parking_lot::{Mutex, RwLock};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering as AtomicOrdering},
        Arc,
    },
    thread,
    time::{self, Instant},
};

lazy_static! {
    /// Current sync phase as an ordinal (matches SyncPhaseType discriminant).
    /// 0 = CatchUpRecoverBlockHeaderFromDB, ..., 5 = Normal.
    /// See docs/flow-audit.md G-CC-3.
    static ref SYNC_PHASE_ORDINAL: Arc<dyn Gauge<usize>> =
        GaugeUsize::register_with_group("sync_phase", "current_ordinal");
    /// Number of phase transitions since startup.
    static ref SYNC_PHASE_TRANSITIONS: Arc<dyn Counter<usize>> =
        CounterUsize::register_with_group("sync_phase", "transitions");
    /// Duration in seconds spent in the previous phase (updated on each
    /// transition).
    static ref SYNC_PHASE_LAST_DURATION_SECS: Arc<dyn Gauge<usize>> =
        GaugeUsize::register_with_group("sync_phase", "last_phase_secs");
}

/// Both Archive and Full node go through the following phases:
///     CatchUpRecoverBlockHeaderFromDB --> CatchUpSyncBlockHeader -->
///     CatchUpCheckpoint --> CatchUpFillBlockBody -->
///     CatchUpSyncBlock --> Normal

#[derive(Debug, Clone, Copy, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub enum SyncPhaseType {
    CatchUpRecoverBlockHeaderFromDB = 0,
    CatchUpSyncBlockHeader = 1,
    CatchUpCheckpoint = 2,
    CatchUpFillBlockBodyPhase = 3,
    CatchUpSyncBlock = 4,
    Normal = 5,
}

pub trait SynchronizationPhaseTrait: Send + Sync {
    fn name(&self) -> &'static str;
    fn phase_type(&self) -> SyncPhaseType;
    fn next(
        &self, _io: &dyn NetworkContext,
        _sync_handler: &SynchronizationProtocolHandler,
    ) -> SyncPhaseType;
    fn start(
        &self, _io: &dyn NetworkContext,
        _sync_handler: &SynchronizationProtocolHandler,
    );
}

pub struct SynchronizationPhaseManagerInner {
    initialized: bool,
    current_phase: SyncPhaseType,
    phases: HashMap<SyncPhaseType, Arc<dyn SynchronizationPhaseTrait>>,
}

impl SynchronizationPhaseManagerInner {
    pub fn new(initial_phase_type: SyncPhaseType) -> Self {
        SynchronizationPhaseManagerInner {
            initialized: false,
            current_phase: initial_phase_type,
            phases: HashMap::new(),
        }
    }

    pub fn register_phase(
        &mut self, phase: Arc<dyn SynchronizationPhaseTrait>,
    ) {
        self.phases.insert(phase.phase_type(), phase);
    }

    pub fn get_phase(
        &self, phase_type: SyncPhaseType,
    ) -> Arc<dyn SynchronizationPhaseTrait> {
        if let Some(p) = self.phases.get(&phase_type) {
            return p.clone();
        }
        // Phases are populated at startup via register_phase(); a miss here
        // means the manager was constructed without all phases registered.
        // Fall back to the conservative initial-recovery phase rather than
        // crashing the protocol handler. If even that is missing we have no
        // safe phase to run with — panic with a clear diagnostic.
        error!(
            "SynchronizationPhaseManager: requested phase {:?} not registered; \
             falling back to CatchUpRecoverBlockHeaderFromDB",
            phase_type
        );
        self.phases
            .get(&SyncPhaseType::CatchUpRecoverBlockHeaderFromDB)
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "SynchronizationPhaseManager has no phases registered; \
                     cannot recover (requested={:?})",
                    phase_type
                )
            })
    }

    pub fn get_current_phase(&self) -> Arc<dyn SynchronizationPhaseTrait> {
        self.get_phase(self.current_phase)
    }

    pub fn change_phase_to(&mut self, phase_type: SyncPhaseType) {
        self.current_phase = phase_type;
    }

    pub fn try_initialize(&mut self) -> bool {
        let initialized = self.initialized;
        if !self.initialized {
            self.initialized = true;
        }

        initialized
    }
}

pub struct SynchronizationPhaseManager {
    inner: RwLock<SynchronizationPhaseManagerInner>,
    /// Instant at which the current phase was entered. Used to compute
    /// SYNC_PHASE_LAST_DURATION_SECS on each transition.
    phase_started_at: Mutex<Instant>,
}

impl SynchronizationPhaseManager {
    pub fn new(
        initial_phase_type: SyncPhaseType,
        sync_state: Arc<SynchronizationState>,
        sync_graph: SharedSynchronizationGraph,
        state_sync: Arc<SnapshotChunkSync>, consensus: Arc<ConsensusGraph>,
    ) -> Self {
        SYNC_PHASE_ORDINAL.update(initial_phase_type as usize);
        let sync_manager = SynchronizationPhaseManager {
            inner: RwLock::new(SynchronizationPhaseManagerInner::new(
                initial_phase_type,
            )),
            phase_started_at: Mutex::new(Instant::now()),
        };

        sync_manager.register_phase(Arc::new(
            CatchUpRecoverBlockHeaderFromDbPhase::new(sync_graph.clone()),
        ));
        sync_manager.register_phase(Arc::new(
            CatchUpSyncBlockHeaderPhase::new(
                sync_state.clone(),
                sync_graph.clone(),
            ),
        ));
        sync_manager
            .register_phase(Arc::new(CatchUpCheckpointPhase::new(state_sync)));
        sync_manager.register_phase(Arc::new(CatchUpFillBlockBodyPhase::new(
            sync_graph.clone(),
        )));
        sync_manager.register_phase(Arc::new(CatchUpSyncBlockPhase::new(
            sync_state.clone(),
            sync_graph.clone(),
        )));
        sync_manager.register_phase(Arc::new(NormalSyncPhase::new(consensus)));

        sync_manager
    }

    pub fn register_phase(&self, phase: Arc<dyn SynchronizationPhaseTrait>) {
        self.inner.write().register_phase(phase);
    }

    pub fn get_phase(
        &self, phase_type: SyncPhaseType,
    ) -> Arc<dyn SynchronizationPhaseTrait> {
        self.inner.read().get_phase(phase_type)
    }

    pub fn get_current_phase(&self) -> Arc<dyn SynchronizationPhaseTrait> {
        self.inner.read().get_current_phase()
    }

    pub fn change_phase_to(
        &self, phase_type: SyncPhaseType, io: &dyn NetworkContext,
        sync_handler: &SynchronizationProtocolHandler,
    ) {
        // Record how long we spent in the previous phase, then stamp the
        // new one.
        {
            let mut started = self.phase_started_at.lock();
            let elapsed = started.elapsed().as_secs() as usize;
            SYNC_PHASE_LAST_DURATION_SECS.update(elapsed);
            *started = Instant::now();
        }
        SYNC_PHASE_ORDINAL.update(phase_type as usize);
        SYNC_PHASE_TRANSITIONS.inc(1);

        self.inner.write().change_phase_to(phase_type);
        let current_phase = self.get_current_phase();
        current_phase.start(io, sync_handler);
    }

    pub fn try_initialize(
        &self, io: &dyn NetworkContext,
        sync_handler: &SynchronizationProtocolHandler,
    ) {
        if !self.inner.write().try_initialize() {
            // if not initialized
            let current_phase = self.get_current_phase();
            current_phase.start(io, sync_handler);
        }
    }
}

pub struct CatchUpRecoverBlockHeaderFromDbPhase {
    pub graph: SharedSynchronizationGraph,
    pub recovered: Arc<AtomicBool>,
}

impl CatchUpRecoverBlockHeaderFromDbPhase {
    pub fn new(graph: SharedSynchronizationGraph) -> Self {
        CatchUpRecoverBlockHeaderFromDbPhase {
            graph,
            recovered: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl SynchronizationPhaseTrait for CatchUpRecoverBlockHeaderFromDbPhase {
    fn name(&self) -> &'static str {
        "CatchUpRecoverBlockHeaderFromDbPhase"
    }

    fn phase_type(&self) -> SyncPhaseType {
        SyncPhaseType::CatchUpRecoverBlockHeaderFromDB
    }

    fn next(
        &self, io: &dyn NetworkContext,
        sync_handler: &SynchronizationProtocolHandler,
    ) -> SyncPhaseType {
        if self.recovered.load(AtomicOrdering::SeqCst) == false {
            return self.phase_type();
        }

        DynamicCapability::ServeHeaders(true).broadcast(io, &sync_handler.syn);
        SyncPhaseType::CatchUpSyncBlockHeader
    }

    fn start(
        &self, _io: &dyn NetworkContext,
        _sync_handler: &SynchronizationProtocolHandler,
    ) {
        info!("start phase {:?}", self.name());
        self.recovered.store(false, AtomicOrdering::SeqCst);
        let recovered = self.recovered.clone();
        let graph = self.graph.clone();
        std::thread::spawn(move || {
            graph.recover_graph_from_db();
            recovered.store(true, AtomicOrdering::SeqCst);
            info!("finish recover header graph from db");
        });
    }
}

pub struct CatchUpSyncBlockHeaderPhase {
    pub syn: Arc<SynchronizationState>,
    pub graph: SharedSynchronizationGraph,
}

impl CatchUpSyncBlockHeaderPhase {
    pub fn new(
        syn: Arc<SynchronizationState>, graph: SharedSynchronizationGraph,
    ) -> Self {
        CatchUpSyncBlockHeaderPhase { syn, graph }
    }
}

impl SynchronizationPhaseTrait for CatchUpSyncBlockHeaderPhase {
    fn name(&self) -> &'static str {
        "CatchUpSyncBlockHeaderPhase"
    }

    fn phase_type(&self) -> SyncPhaseType {
        SyncPhaseType::CatchUpSyncBlockHeader
    }

    fn next(
        &self, io: &dyn NetworkContext,
        sync_handler: &SynchronizationProtocolHandler,
    ) -> SyncPhaseType {
        let median_epoch = match self.syn.median_epoch_from_normal_peers() {
            None => {
                return if self.syn.allow_phase_change_without_peer() {
                    SyncPhaseType::CatchUpCheckpoint
                } else {
                    self.phase_type()
                }
            }
            Some(epoch) => epoch,
        };
        debug!(
            "best_epoch: {}, peer median: {}",
            self.graph.consensus.best_epoch_number(),
            median_epoch
        );
        if self.graph.consensus.catch_up_completed(median_epoch) {
            if normal_peers_share_known_terminal(&self.syn, &self.graph) {
                return SyncPhaseType::CatchUpCheckpoint;
            }
            sync_handler.request_missing_terminals(io);
        }

        self.phase_type()
    }

    fn start(
        &self, io: &dyn NetworkContext,
        sync_handler: &SynchronizationProtocolHandler,
    ) {
        info!("start phase {:?}", self.name());
        let (_, cur_era_genesis_height) =
            self.graph.get_genesis_hash_and_height_in_current_era();
        *sync_handler.latest_epoch_requested.lock() =
            (cur_era_genesis_height, Instant::now(), 0, 0);

        // sync block headers from peers
        sync_handler.request_epochs(io);
    }
}

pub struct CatchUpCheckpointPhase {
    state_sync: Arc<SnapshotChunkSync>,

    /// Is `true` if we have the state locally and do not need to sync
    /// checkpoints. Only set when the phase starts.
    has_state: AtomicBool,
}

impl CatchUpCheckpointPhase {
    pub fn new(state_sync: Arc<SnapshotChunkSync>) -> Self {
        CatchUpCheckpointPhase {
            state_sync,
            has_state: AtomicBool::new(false),
        }
    }
}

impl SynchronizationPhaseTrait for CatchUpCheckpointPhase {
    fn name(&self) -> &'static str {
        "CatchUpCheckpointPhase"
    }

    fn phase_type(&self) -> SyncPhaseType {
        SyncPhaseType::CatchUpCheckpoint
    }

    fn next(
        &self, io: &dyn NetworkContext,
        sync_handler: &SynchronizationProtocolHandler,
    ) -> SyncPhaseType {
        if self.has_state.load(AtomicOrdering::SeqCst) {
            return SyncPhaseType::CatchUpFillBlockBodyPhase;
        }
        let epoch_to_sync = sync_handler.graph.consensus.get_to_sync_epoch_id();
        let current_era_genesis = sync_handler
            .graph
            .data_man
            .get_cur_consensus_era_genesis_hash();
        self.state_sync.update_status(
            current_era_genesis,
            epoch_to_sync,
            io,
            sync_handler,
        );
        if sync_handler.syn.allow_phase_change_without_peer()
            && sync_handler.syn.peers.read().is_empty()
            && self.state_sync.status() == Status::Inactive
        {
            warn!(
                "No peers available for checkpoint sync at {:?}; continuing with local body sync",
                epoch_to_sync
            );
            *sync_handler.synced_epoch_id.lock() = None;
            return SyncPhaseType::CatchUpFillBlockBodyPhase;
        }
        if self.state_sync.status() == Status::Invalid {
            warn!(
                "Checkpoint sync is unavailable for {:?}; falling back to legacy body sync",
                epoch_to_sync
            );
            *sync_handler.synced_epoch_id.lock() = None;
            return SyncPhaseType::CatchUpFillBlockBodyPhase;
        }
        if self.state_sync.status() == Status::Completed {
            self.state_sync.restore_execution_state(sync_handler);
            *sync_handler.synced_epoch_id.lock() = Some(epoch_to_sync);
            SyncPhaseType::CatchUpFillBlockBodyPhase
        } else {
            self.phase_type()
        }
    }

    fn start(
        &self, io: &dyn NetworkContext,
        sync_handler: &SynchronizationProtocolHandler,
    ) {
        info!("start phase {:?}", self.name());
        self.state_sync.reset();
        self.has_state.store(false, AtomicOrdering::SeqCst);
        *sync_handler.synced_epoch_id.lock() = None;
        sync_handler.graph.inner.write().locked_for_catchup = true;
        while sync_handler.graph.is_consensus_worker_busy() {
            //Thread sleep for 10ms to avoid busy-waiting
            thread::sleep(time::Duration::from_millis(1));
        }
        let current_era_genesis = sync_handler
            .graph
            .data_man
            .get_cur_consensus_era_genesis_hash();
        let epoch_to_sync = sync_handler.graph.consensus.get_to_sync_epoch_id();

        if let Some(commitment) =
            load_checkpoint_state_if_available(sync_handler, &epoch_to_sync)
        {
            info!("CatchUpCheckpointPhase: commitment for epoch {:?} exists, skip state sync. \
                commitment={:?}", epoch_to_sync, commitment);
            self.has_state.store(true, AtomicOrdering::SeqCst);

            // TODO Here has_state could mean we have the snapshot of the state
            // or the last snapshot and the delta mpt. We only need to specially
            // handle the case of snapshot-only state where we
            // cannot compute state_valid because we do not have a
            // valid state root.
            if epoch_to_sync != sync_handler.graph.data_man.true_genesis.hash()
            {
                *sync_handler.synced_epoch_id.lock() = Some(epoch_to_sync);
            }
            return;
        }

        self.state_sync.update_status(
            current_era_genesis,
            epoch_to_sync,
            io,
            sync_handler,
        );
    }
}

pub struct CatchUpFillBlockBodyPhase {
    pub graph: SharedSynchronizationGraph,
}

impl CatchUpFillBlockBodyPhase {
    pub fn new(graph: SharedSynchronizationGraph) -> Self {
        CatchUpFillBlockBodyPhase { graph }
    }
}

impl SynchronizationPhaseTrait for CatchUpFillBlockBodyPhase {
    fn name(&self) -> &'static str {
        "CatchUpFillBlockBodyPhase"
    }

    fn phase_type(&self) -> SyncPhaseType {
        SyncPhaseType::CatchUpFillBlockBodyPhase
    }

    fn next(
        &self, io: &dyn NetworkContext,
        sync_handler: &SynchronizationProtocolHandler,
    ) -> SyncPhaseType {
        if self.graph.is_fill_block_completed() {
            if self.graph.complete_filling_block_bodies() {
                return SyncPhaseType::CatchUpSyncBlock;
            } else {
                // consensus graph is reconstructed and we need to request more
                // bodies
                sync_handler.request_block_bodies(io)
            }
        }
        self.phase_type()
    }

    fn start(
        &self, io: &dyn NetworkContext,
        sync_handler: &SynchronizationProtocolHandler,
    ) {
        info!("start phase {:?}", self.name());
        {
            // TODO: remove this after the full state is enabled
            let full_state_start_height = Some(0u64);
            let full_state_space = self
                .graph
                .data_man
                .storage_manager
                .config()
                .single_mpt_space;
            // For both archive and full node, synced_epoch_id possible be
            // `None`. It wil be none when stable epoch is equal to
            // true genesis In both cases, we should set
            // `state_availability_boundary` to
            // `[cur_era_stable_height, cur_era_stable_height]`.
            if let Some(epoch_synced) = &*sync_handler.synced_epoch_id.lock() {
                let epoch_synced_height = match self
                    .graph
                    .data_man
                    .block_header_by_hash(epoch_synced)
                {
                    Some(h) => h.height(),
                    None => {
                        error!(
                            "CatchUpFillBlockBodyPhase: missing header for \
                             synced checkpoint {:?}; aborting phase start so \
                             the sync state machine can re-derive the \
                             checkpoint",
                            epoch_synced
                        );
                        return;
                    }
                };
                *self.graph.data_man.state_availability_boundary.write() =
                    StateAvailabilityBoundary::new(
                        *epoch_synced,
                        epoch_synced_height,
                        full_state_start_height,
                        full_state_space,
                    );
                self.graph
                    .data_man
                    .state_availability_boundary
                    .write()
                    .set_synced_state_height(epoch_synced_height);
            } else {
                let cur_era_stable_hash =
                    self.graph.data_man.get_cur_consensus_era_stable_hash();
                let cur_era_stable_height = match self
                    .graph
                    .data_man
                    .block_header_by_hash(&cur_era_stable_hash)
                {
                    Some(h) => h.height(),
                    None => {
                        error!(
                            "CatchUpFillBlockBodyPhase: missing stable era \
                             header {:?}; aborting phase start",
                            cur_era_stable_hash
                        );
                        return;
                    }
                };
                *self.graph.data_man.state_availability_boundary.write() =
                    StateAvailabilityBoundary::new(
                        cur_era_stable_hash,
                        cur_era_stable_height,
                        full_state_start_height,
                        full_state_space,
                    );
            }
            self.graph.data_man.sample_state_availability_metrics();
            self.graph.inner.write().block_to_fill_set =
                self.graph.consensus.get_blocks_needing_bodies();
            sync_handler.request_block_bodies(io);
        }
    }
}

pub struct CatchUpSyncBlockPhase {
    pub syn: Arc<SynchronizationState>,
    pub graph: SharedSynchronizationGraph,
}

impl CatchUpSyncBlockPhase {
    pub fn new(
        syn: Arc<SynchronizationState>, graph: SharedSynchronizationGraph,
    ) -> Self {
        CatchUpSyncBlockPhase { syn, graph }
    }
}

impl SynchronizationPhaseTrait for CatchUpSyncBlockPhase {
    fn name(&self) -> &'static str {
        "CatchUpSyncBlockPhase"
    }

    fn phase_type(&self) -> SyncPhaseType {
        SyncPhaseType::CatchUpSyncBlock
    }

    fn next(
        &self, io: &dyn NetworkContext,
        sync_handler: &SynchronizationProtocolHandler,
    ) -> SyncPhaseType {
        let median_epoch = match self.syn.median_epoch_from_normal_peers() {
            None => {
                return if self.syn.allow_phase_change_without_peer() {
                    sync_handler.graph.consensus.enter_normal_phase();
                    SyncPhaseType::Normal
                } else {
                    self.phase_type()
                }
            }
            Some(epoch) => epoch,
        };
        // Use the median normal-peer epoch as a conservative exit signal.
        // Normal sync will return to catch-up if the node falls behind again.
        if self.graph.consensus.best_epoch_number()
            + CATCH_UP_EPOCH_LAG_THRESHOLD
            >= median_epoch
        {
            if normal_peers_share_known_terminal(&self.syn, &self.graph) {
                sync_handler.graph.consensus.enter_normal_phase();
                return SyncPhaseType::Normal;
            }
            sync_handler.request_missing_terminals(io);
        }

        self.phase_type()
    }

    fn start(
        &self, io: &dyn NetworkContext,
        sync_handler: &SynchronizationProtocolHandler,
    ) {
        info!("start phase {:?}", self.name());
        let (_, cur_era_genesis_height) =
            self.graph.get_genesis_hash_and_height_in_current_era();
        *sync_handler.latest_epoch_requested.lock() =
            (cur_era_genesis_height, Instant::now(), 0, 0);

        sync_handler.request_epochs(io);
    }
}

pub struct NormalSyncPhase {
    _consensus: Arc<ConsensusGraph>,
}

impl NormalSyncPhase {
    pub fn new(consensus: Arc<ConsensusGraph>) -> Self {
        NormalSyncPhase {
            _consensus: consensus,
        }
    }
}

impl SynchronizationPhaseTrait for NormalSyncPhase {
    fn name(&self) -> &'static str {
        "NormalSyncPhase"
    }

    fn phase_type(&self) -> SyncPhaseType {
        SyncPhaseType::Normal
    }

    fn next(
        &self, _io: &dyn NetworkContext,
        sync_handler: &SynchronizationProtocolHandler,
    ) -> SyncPhaseType {
        if let Some(median_epoch) =
            sync_handler.syn.median_epoch_from_normal_peers()
        {
            if sync_handler.graph.consensus.best_epoch_number()
                + CATCH_UP_EPOCH_LAG_THRESHOLD
                < median_epoch
            {
                sync_handler.graph.consensus.leave_normal_phase();
                return SyncPhaseType::CatchUpSyncBlock;
            }
        }
        self.phase_type()
    }

    fn start(
        &self, io: &dyn NetworkContext,
        sync_handler: &SynchronizationProtocolHandler,
    ) {
        info!("start phase {:?}", self.name());
        sync_handler.request_missing_terminals(io);
    }
}

fn normal_peers_share_known_terminal(
    syn: &SynchronizationState, graph: &SharedSynchronizationGraph,
) -> bool {
    let peers = syn.peers.read();
    let mut saw_normal_peer = false;

    for (_, peer_lock) in peers.iter() {
        let peer = peer_lock.read();
        if !peer
            .capabilities
            .contains(DynamicCapability::NormalPhase(true))
        {
            continue;
        }
        saw_normal_peer = true;
        if peer
            .latest_block_hashes
            .iter()
            .any(|hash| graph.contains_block_header(hash))
        {
            return true;
        }
    }

    !saw_normal_peer
}

fn load_checkpoint_state_if_available(
    sync_handler: &SynchronizationProtocolHandler, epoch_to_sync: &H256,
) -> Option<EpochExecutionCommitment> {
    let data_man = &sync_handler.graph.data_man;
    let storage_manager = data_man.storage_manager.get_storage_manager();
    if storage_manager
        .get_snapshot_info_at_epoch(epoch_to_sync)
        .is_none()
    {
        debug!(
            "Checkpoint {:?} has execution commitment but no snapshot metadata; resync state",
            epoch_to_sync
        );
        return None;
    }

    let snapshot_manager = storage_manager.get_snapshot_manager();
    match snapshot_manager.get_snapshot_by_epoch_id(epoch_to_sync, true, false)
    {
        Ok(Some(_)) => {}
        Ok(None) => {
            warn!(
                "Checkpoint {:?} has execution commitment but no snapshot db; resync state",
                epoch_to_sync
            );
            return None;
        }
        Err(err) => {
            warn!(
                "Checkpoint {:?} snapshot open failed: {}; resync state",
                epoch_to_sync, err
            );
            return None;
        }
    }

    data_man.load_epoch_execution_commitment_from_db(epoch_to_sync)
}
