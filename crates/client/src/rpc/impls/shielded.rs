// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::rpc::{
    error_codes::invalid_params,
    traits::shielded::ShieldedServer,
    types::{
        shielded::{ShieldedSection, ShieldedStatus, ShieldedTxInfo},
        Bytes,
    },
};
use mazze_executor::spec::{shielded_to_mazzy, SHIELDED_UNIT};
use mazze_executor::shielded_engine::{
    backing_account, tree_state, ShieldedOutcome,
};
use mazze_rpc_error::Result as JsonRpcResult;
use mazze_shielded_pq::field::to_bytes;
use mazze_types::{H256, U256, U64};
use mazzecore::{SharedConsensusGraph, SharedTransactionPool};
use primitives::{
    shielded::{ShieldedDigest, ShieldedTxBody},
    EpochNumber,
};

/// Sections per `shielded_getSections` call.
pub const MAX_SECTIONS_PER_CALL: u64 = 1000;

pub struct ShieldedHandler {
    consensus: SharedConsensusGraph,
    tx_pool: SharedTransactionPool,
}

impl ShieldedHandler {
    pub fn new(
        consensus: SharedConsensusGraph, tx_pool: SharedTransactionPool,
    ) -> Self {
        ShieldedHandler { consensus, tx_pool }
    }
}

fn digest_bytes(d: &ShieldedDigest) -> Bytes { Bytes(d.as_bytes().to_vec()) }

impl ShieldedServer for ShieldedHandler {
    fn shielded_send_raw_transaction(
        &self, body: Bytes, proof: Bytes,
    ) -> JsonRpcResult<H256> {
        let body = ShieldedTxBody::decode_canonical(&body.0)
            .map_err(|e| invalid_params("body", e))?;
        let hash = self
            .tx_pool
            .insert_shielded(body, proof.0, true)
            .map_err(|e| invalid_params("transaction", e))?;
        self.tx_pool.shielded.mark_originated(hash);
        Ok(hash)
    }

    fn shielded_get_status(&self) -> JsonRpcResult<ShieldedStatus> {
        let params = self.tx_pool.machine().params().shielded.clone();
        let state = self.tx_pool.best_executed_state_view();
        let db_err = |e| invalid_params("state", format!("{:?}", e));
        let (root, leaves) = tree_state(&state).map_err(db_err)?;
        let backing = state.balance(&backing_account()).map_err(db_err)?;
        Ok(ShieldedStatus {
            active: params.activation_height != u64::MAX,
            activation_height: params.activation_height.into(),
            slot_epochs: params.slot_epochs.into(),
            max_txs_per_block: (params.max_txs_per_block as u64).into(),
            tx_fee: shielded_to_mazzy(params.tx_fee),
            amount_unit: SHIELDED_UNIT.into(),
            anchor_window_epochs: params.anchor_window_epochs.into(),
            chain_id: (self.consensus.best_chain_id().in_shielded_space() as u64)
                .into(),
            root: Bytes(to_bytes(&root)),
            leaves: leaves.into(),
            backing,
            pool_size: (self.tx_pool.shielded.pending_len() as u64).into(),
            executed_epoch: self.consensus.best_executed_state_epoch_number().into(),
        })
    }

    fn shielded_get_sections(
        &self, from_epoch: U64, count: U64,
    ) -> JsonRpcResult<Vec<ShieldedSection>> {
        let params = self.tx_pool.machine().params().shielded.clone();
        let executed = self.consensus.best_executed_state_epoch_number();
        let from = from_epoch.as_u64();
        let to = from
            .saturating_add(count.as_u64().min(MAX_SECTIONS_PER_CALL))
            .min(executed + 1);
        let data_man = self.consensus.get_data_manager();
        let mut sections = Vec::new();
        for epoch in from..to {
            if !params.is_slot(epoch) {
                continue;
            }
            // From the history column, never from block bodies: it is kept
            // on every node, whatever its block retention.
            let pivot = self
                .consensus
                .get_hash_from_epoch_number(EpochNumber::Number(epoch))
                .ok();
            let (hash, entry) =
                match data_man.shielded_history(epoch, pivot.as_ref()) {
                    Some(found) => found,
                    None => continue,
                };
            let transactions = entry
                .bodies
                .iter()
                .zip(&entry.outcomes)
                .map(|(body, outcome)| {
                    let (applied, positions, skip_reason) = match outcome {
                        ShieldedOutcome::Applied { positions } => (
                            true,
                            Some([positions[0].into(), positions[1].into()]),
                            None,
                        ),
                        ShieldedOutcome::Skipped(reason) => {
                            (false, None, Some(format!("{:?}", reason)))
                        }
                    };
                    ShieldedTxInfo {
                        body_hash: body.hash(),
                        anchor: digest_bytes(&body.anchor),
                        nullifiers: [digest_bytes(&body.nf[0]), digest_bytes(&body.nf[1])],
                        output_commitments: [
                            digest_bytes(&body.cm_out[0]),
                            digest_bytes(&body.cm_out[1]),
                        ],
                        recovery_tag: Bytes(body.recovery.tag.to_vec()),
                        recovery_slot: Bytes(body.recovery.slot.to_vec()),
                        recovery_mac: Bytes(body.recovery.mac.to_vec()),
                        v_public_in: shielded_to_mazzy(body.v_public_in),
                        v_public_out: shielded_to_mazzy(body.v_public_out),
                        fee: shielded_to_mazzy(body.fee),
                        applied,
                        positions,
                        skip_reason,
                    }
                })
                .collect();
            sections.push(ShieldedSection {
                epoch: epoch.into(),
                block_hash: hash,
                transactions,
            });
        }
        Ok(sections)
    }
}
