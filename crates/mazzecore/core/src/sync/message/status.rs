// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Single-version pre-launch sync handshake — `StatusV3` carries the
//! peer's chain id, genesis, best epoch, advertised node type, and
//! terminal block hashes. The older `StatusV2` (no node_type) has
//! been dropped as part of the protocol-consolidation pass.

use crate::{
    sync::{
        message::{
            handleable::{Context, Handleable},
            DynamicCapability,
        },
        Error, ErrorKind, SynchronizationPeerState,
    },
    NodeType,
};
use mazze_internal_common::ChainIdParamsDeprecated;
use mazze_types::H256;
use network::{
    NODE_TAG_ARCHIVE, NODE_TAG_FULL, NODE_TAG_NODE_TYPE, NODE_TAG_NODE_VERSION,
};
use rlp_derive::{RlpDecodable, RlpEncodable};
use std::{collections::HashSet, time::Instant};
use throttling::token_bucket::TokenBucketManager;

/// What software a peer runs. Appended to `StatusV3` as an optional trailing
/// field: older nodes ignore it (the RLP decoder reads fields by index) and
/// newer nodes see `Default` (consensus_version 0 = unknown) from older ones,
/// so mixed-version fleets keep working during a rolling deploy.
#[derive(Debug, Clone, Default, PartialEq, RlpDecodable, RlpEncodable)]
pub struct NodeVersionInfo {
    /// `mazze_parameters::consensus::CONSENSUS_VERSION`; 0 = not announced.
    pub consensus_version: u32,
    /// Node software version, e.g. "2.2.0".
    pub node_version: String,
    /// Short commit the binary was built from.
    pub commit: String,
}

impl NodeVersionInfo {
    pub fn local() -> Self {
        NodeVersionInfo {
            consensus_version: mazze_parameters::consensus::CONSENSUS_VERSION,
            node_version: env!("CARGO_PKG_VERSION").to_string(),
            commit: parity_version::commit().to_string(),
        }
    }

    pub fn display(&self) -> String {
        if self.consensus_version == 0 {
            "unknown (pre-2.2.0)".into()
        } else {
            format!(
                "{}-{} (consensus v{})",
                self.node_version, self.commit, self.consensus_version
            )
        }
    }
}

#[derive(Debug, PartialEq, RlpDecodable, RlpEncodable)]
pub struct StatusV3 {
    pub chain_id: ChainIdParamsDeprecated,
    pub genesis_hash: H256,
    pub best_epoch: u64,
    pub node_type: NodeType,
    pub terminal_block_hashes: Vec<H256>,
    #[rlp(default)]
    pub version: NodeVersionInfo,
}

impl Handleable for StatusV3 {
    fn handle(self, ctx: &Context) -> Result<(), Error> {
        debug!("on_status, msg=:{:?}", self);

        let chain_id = ctx
            .manager
            .graph
            .consensus
            .get_config()
            .chain_id
            .read()
            .to_native_space_params();
        if !chain_id.matches(&self.chain_id.clone().into(), self.best_epoch) {
            debug!(
                "Peer {:?} chain_id mismatches (ours: {:?}, theirs: {:?})",
                ctx.node_id, chain_id, self.chain_id,
            );
            bail!(ErrorKind::InvalidStatus("chain_id mismatches".into()));
        }
        drop(chain_id);

        let genesis_hash = ctx.manager.graph.data_man.true_genesis.hash();
        if ctx.manager.protocol_config.check_status_genesis {
            if genesis_hash != self.genesis_hash {
                debug!(
                "Peer {:?} genesis hash mismatches (ours: {:?}, theirs: {:?})",
                ctx.node_id, genesis_hash, self.genesis_hash
            );
                bail!(ErrorKind::InvalidStatus(
                    "genesis hash mismatches".into()
                ));
            }
        }

        // Different consensus rules = different chain: refuse the peer with a
        // clear reason instead of failing later on invalid blocks. 0 means the
        // peer predates version announcements and is accepted.
        let ours = mazze_parameters::consensus::CONSENSUS_VERSION;
        if self.version.consensus_version != 0
            && self.version.consensus_version != ours
        {
            warn!(
                "Peer {:?} runs consensus v{} ({}), ours is v{}; rejecting",
                ctx.node_id,
                self.version.consensus_version,
                self.version.display(),
                ours
            );
            bail!(ErrorKind::InvalidStatus(
                "consensus version mismatches".into()
            ));
        }
        ctx.insert_peer_node_tag(
            ctx.node_id(),
            NODE_TAG_NODE_VERSION,
            &self.version.display(),
        );

        let latest: HashSet<H256> =
            self.terminal_block_hashes.iter().cloned().collect();

        match self.node_type {
            NodeType::Archive => {
                let key: String = NODE_TAG_NODE_TYPE.into();
                let value: String = NODE_TAG_ARCHIVE.into();
                ctx.insert_peer_node_tag(ctx.node_id(), &key, &value);
            }
            NodeType::Full | NodeType::FullFast => {
                let key: String = NODE_TAG_NODE_TYPE.into();
                let value: String = NODE_TAG_FULL.into();
                ctx.insert_peer_node_tag(ctx.node_id(), &key, &value);
            }
            _ => {}
        };

        if let Ok(peer_info) = ctx.manager.syn.get_peer_info(&ctx.node_id) {
            let latest_updated = {
                let mut peer_info = peer_info.write();
                peer_info.update(Some(self.node_type), latest, self.best_epoch)
            };

            if latest_updated {
                ctx.manager.start_sync(ctx.io);
            }
        } else {
            let peer_protocol_version =
                match ctx.manager.syn.on_status_in_handshaking(&ctx.node_id) {
                    None => {
                        warn!(
                            "Unexpected Status message from peer={}",
                            ctx.node_id
                        );
                        return Err(ErrorKind::UnknownPeer.into());
                    }
                    Some(protocol_version) => protocol_version,
                };

            let throttling =
                match ctx.manager.protocol_config.throttling_config_file {
                    Some(ref file) => {
                        TokenBucketManager::load(file, Some("sync_protocol"))
                            .expect("invalid throttling configuration file")
                    }
                    None => TokenBucketManager::default(),
                };

            let mut peer_state = SynchronizationPeerState {
                node_id: ctx.node_id(),
                node_type: self.node_type,
                is_validator: false,
                protocol_version: peer_protocol_version,
                genesis_hash,
                best_epoch: self.best_epoch,
                latest_block_hashes: latest,
                received_transaction_count: 0,
                heartbeat: Instant::now(),
                capabilities: Default::default(),
                notified_capabilities: Default::default(),
                throttling,
                throttled_msgs: Default::default(),
            };

            peer_state
                .capabilities
                .insert(DynamicCapability::NormalPhase(true));

            debug!(
                "New peer (pv={:?}, gh={:?})",
                peer_protocol_version, self.genesis_hash
            );

            debug!("Peer {:?} connected", ctx.node_id);
            ctx.manager
                .syn
                .peer_connected(ctx.node_id.clone(), peer_state);
            ctx.manager.request_manager.on_peer_connected(&ctx.node_id);

            ctx.manager.start_sync(ctx.io);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `StatusV3` exactly as nodes before 2.2.0 encode/decode it.
    #[derive(Debug, PartialEq, RlpDecodable, RlpEncodable)]
    struct StatusV3Pre220 {
        chain_id: ChainIdParamsDeprecated,
        genesis_hash: H256,
        best_epoch: u64,
        node_type: NodeType,
        terminal_block_hashes: Vec<H256>,
    }

    fn sample_new() -> StatusV3 {
        StatusV3 {
            chain_id: ChainIdParamsDeprecated { chain_id: 1990 },
            genesis_hash: H256::from_low_u64_be(7),
            best_epoch: 123,
            node_type: NodeType::Archive,
            terminal_block_hashes: vec![H256::from_low_u64_be(9)],
            version: NodeVersionInfo::local(),
        }
    }

    #[test]
    fn old_node_decodes_new_status() {
        let bytes = rlp::encode(&sample_new());
        let old: StatusV3Pre220 = rlp::decode(&bytes).expect("old decoder");
        assert_eq!(old.best_epoch, 123);
        assert_eq!(old.terminal_block_hashes.len(), 1);
    }

    #[test]
    fn new_node_decodes_old_status() {
        let n = sample_new();
        let old = StatusV3Pre220 {
            chain_id: n.chain_id.clone(),
            genesis_hash: n.genesis_hash,
            best_epoch: n.best_epoch,
            node_type: n.node_type,
            terminal_block_hashes: n.terminal_block_hashes.clone(),
        };
        let decoded: StatusV3 = rlp::decode(&rlp::encode(&old)).expect("new decoder");
        assert_eq!(decoded.version, NodeVersionInfo::default());
        assert_eq!(decoded.version.consensus_version, 0);
    }

    #[test]
    fn new_status_roundtrip_keeps_version() {
        let n = sample_new();
        let decoded: StatusV3 = rlp::decode(&rlp::encode(&n)).unwrap();
        assert_eq!(decoded, n);
        assert_eq!(
            decoded.version.consensus_version,
            mazze_parameters::consensus::CONSENSUS_VERSION
        );
    }
}
