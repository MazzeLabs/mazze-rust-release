// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::rpc::{
    error_codes::invalid_params,
    traits::shielded::ShieldedServer,
    types::{
        shielded::{
            ShieldedSection, ShieldedStateAt, ShieldedStatus, ShieldedTxInfo,
            ShieldedWitnessInfo,
        },
        Bytes,
    },
};
use mazze_executor::spec::{shielded_to_mazzy, SHIELDED_UNIT};
use mazze_executor::shielded_engine::{
    backing_account, pending_credits, tree_state, turnstile, ShieldedOutcome,
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
        let (hash, new) = self
            .tx_pool
            .insert_shielded(
                body,
                proof.0,
                mazzecore::transaction_pool::shielded::ShieldedSource::Rpc,
            )
            .map_err(|e| invalid_params("transaction", e))?;
        // A transaction the pool already held is acknowledged the same way,
        // but not sent into a new stem.
        if new {
            self.tx_pool.shielded.mark_originated(hash);
        }
        Ok(hash)
    }

    fn shielded_get_status(&self) -> JsonRpcResult<ShieldedStatus> {
        let params = self.tx_pool.machine().params().shielded.clone();
        let state = self.tx_pool.best_executed_state_view();
        let db_err = |e| invalid_params("state", format!("{:?}", e));
        let (root, leaves) = tree_state(&state).map_err(db_err)?;
        let backing = state.balance(&backing_account()).map_err(db_err)?;
        Ok(ShieldedStatus {
            // Activated and not paused: a wallet must not deposit into a
            // lane that will not execute.
            active: params.is_active(self.consensus.best_epoch_number()),
            pause_height: params.pause_height.into(),
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

    fn shielded_get_state_at(&self, epoch: U64) -> JsonRpcResult<ShieldedStateAt> {
        let db_err = |e| invalid_params("state", format!("{:?}", e));
        let state_db = self
            .consensus
            .get_state_db_by_epoch_number(EpochNumber::Number(epoch.as_u64()), "epoch")?;
        let state = mazze_executor::state::State::new(state_db).map_err(db_err)?;
        let (root, leaves) = tree_state(&state).map_err(db_err)?;
        let (total_in, total_out, total_burnt) = turnstile(&state).map_err(db_err)?;
        Ok(ShieldedStateAt {
            epoch,
            root: Bytes(to_bytes(&root)),
            leaves: leaves.into(),
            backing: state.balance(&backing_account()).map_err(db_err)?,
            total_in,
            total_out,
            total_burnt,
            pending_credits: pending_credits(&state).map_err(db_err)?,
        })
    }

    fn shielded_get_witness(
        &self, block_hash: H256, index: U64,
    ) -> JsonRpcResult<ShieldedWitnessInfo> {
        // The body section, then this one proof: never the whole witness
        // (up to 32 proofs of ~416 KB) for one call on a public port.
        let data_man = self.consensus.get_data_manager();
        let unknown = || invalid_params("blockHash", "unknown block");
        let header = data_man.block_header_by_hash(&block_hash).ok_or_else(unknown)?;
        let (_, shielded) =
            data_man.db_manager.block_body_from_db(&block_hash).ok_or_else(unknown)?;
        let i = index.as_u64() as usize;
        let body = shielded
            .get(i)
            .ok_or_else(|| invalid_params("index", "no such shielded transaction"))?;
        Ok(ShieldedWitnessInfo {
            body: Bytes(rlp::encode(&**body).to_vec()),
            proof: data_man
                .db_manager
                .block_witness_proof_from_db(header.height(), &block_hash, i)
                .map(Bytes),
        })
    }
}
