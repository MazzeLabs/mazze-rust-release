use crate::{
    message::{
        GetMaybeRequestId, Message, MessageProtocolVersionBound, MsgId,
        RequestId, SetRequestId,
    },
    sync::{
        message::{
            msgid, Context, DynamicCapability, Handleable, KeyContainer,
            StateSyncCandidateResponse,
        },
        request_manager::{AsAny, Request},
        state::storage::SnapshotSyncCandidate,
        Error, ProtocolConfiguration, SYNC_PROTO_V1, SYNC_PROTO_V4,
    },
};
use lazy_static::lazy_static;
use malloc_size_of_derive::MallocSizeOf as DeriveMallocSizeOf;
use mazze_storage::storage_db::SnapshotDbManagerTrait;
use metrics::{register_meter_with_group, Meter};
use network::service::ProtocolVersion;
use rlp::Encodable;
use rlp_derive::{RlpDecodable, RlpEncodable};
use std::{any::Any, sync::Arc, time::Duration};

lazy_static! {
    /// C.2 — In-memory `snapshot_info_map_by_epoch` says we have a
    /// snapshot for this epoch, but its on-disk directory is missing.
    /// Indicates either a crash between checkpoint write and snapshot
    /// materialisation, or a snapshot that was deleted out from under
    /// us. We MUST NOT advertise it as available; doing so would lead
    /// peers to download a manifest we can't service.
    ///
    /// See `docs/checkpoint-snapshot-lifecycle.md` Phase C.2.
    static ref SNAPSHOT_ADVERTISED_BUT_MISSING_TOTAL: Arc<dyn Meter> =
        register_meter_with_group("snapshot", "advertised_but_missing_total");
}

#[derive(Clone, RlpEncodable, RlpDecodable, Debug, DeriveMallocSizeOf)]
pub struct StateSyncCandidateRequest {
    pub request_id: RequestId,
    pub candidates: Vec<SnapshotSyncCandidate>,
}

build_msg_with_request_id_impl! {
    StateSyncCandidateRequest, msgid::STATE_SYNC_CANDIDATE_REQUEST,
    "StateSyncCandidateRequest", SYNC_PROTO_V1, SYNC_PROTO_V4
}

impl Handleable for StateSyncCandidateRequest {
    fn handle(self, ctx: &Context) -> Result<(), Error> {
        let mut supported_candidates =
            Vec::with_capacity(self.candidates.len());
        let storage_manager = ctx
            .manager
            .graph
            .data_man
            .storage_manager
            .get_storage_manager();
        for candidate in self.candidates {
            match candidate {
                SnapshotSyncCandidate::FullSync {
                    height,
                    snapshot_epoch_id,
                } => {
                    match storage_manager
                        .get_snapshot_info_at_epoch(&snapshot_epoch_id)
                    {
                        Some(snapshot_info) => {
                            if snapshot_info.height != height {
                                warn!(
                                    "Invalid SnapshotSyncCandidate, height unmatch: get {:?}, \
                                    local_height of the snapshot is {}",
                                    candidate, snapshot_info.height);
                            } else if !storage_manager
                                .get_snapshot_manager()
                                .get_snapshot_db_manager()
                                .snapshot_dir_exists(&snapshot_epoch_id)
                            {
                                // C.2 — in-memory map advertises this
                                // snapshot but the on-disk directory
                                // isn't there. Either a crash between
                                // `set_cur_consensus_era_genesis_hash`
                                // and snapshot finalisation, or a
                                // pruning race. Refuse to advertise.
                                SNAPSHOT_ADVERTISED_BUT_MISSING_TOTAL
                                    .mark(1);
                                warn!(
                                    "C.2 guard: refusing to advertise snapshot {:?} \
                                     (height={}) — snapshot_info_map says present but on-disk \
                                     directory is missing. Likely a checkpoint↔snapshot \
                                     atomicity gap; this node should not be a snapshot-sync \
                                     source for this epoch until the snapshot is rebuilt.",
                                    snapshot_epoch_id, height
                                );
                            } else {
                                supported_candidates.push(
                                    SnapshotSyncCandidate::FullSync {
                                        height,
                                        snapshot_epoch_id,
                                    },
                                );
                            }
                        }
                        None => {
                            debug!(
                                "Requested snapshot not exist: {:?}",
                                candidate
                            );
                        }
                    }
                }
                _ => {
                    warn!("Unsupported candidate: {:?}", candidate);
                }
            }
        }
        ctx.send_response(&StateSyncCandidateResponse {
            request_id: self.request_id,
            supported_candidates,
        })?;

        Ok(())
    }
}

impl AsAny for StateSyncCandidateRequest {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl Request for StateSyncCandidateRequest {
    fn timeout(&self, conf: &ProtocolConfiguration) -> Duration {
        conf.snapshot_candidate_request_timeout
    }

    fn on_removed(&self, _inflight_keys: &KeyContainer) {}

    fn with_inflight(&mut self, _inflight_keys: &KeyContainer) {}

    fn is_empty(&self) -> bool {
        false
    }

    fn resend(&self) -> Option<Box<dyn Request>> {
        None
    }

    fn required_capability(&self) -> Option<DynamicCapability> {
        None
    }
}
