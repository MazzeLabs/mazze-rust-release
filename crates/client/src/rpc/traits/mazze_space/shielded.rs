// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! The shielded space RPC (docs/internal/privacy-pq-stark.md §9.7.1). The
//! node serves public data only: it never sees a key, a note opening or a
//! Merkle path, and there is deliberately no `getTreePath`.

use crate::rpc::types::{
    shielded::{
        ShieldedSection, ShieldedStateAt, ShieldedStatus, ShieldedWitnessInfo,
    },
    Bytes,
};
use jsonrpsee::proc_macros::rpc;
use mazze_rpc_error::Result as JsonRpcResult;
use mazze_types::{H256, U64};

#[rpc(server)]
pub trait Shielded {
    /// Admits a shielded transaction (its canonical body and its proof) and
    /// sends it into a Dandelion++ stem. Returns the body hash.
    #[method(name = "shielded_sendRawTransaction", blocking)]
    fn shielded_send_raw_transaction(
        &self, body: Bytes, proof: Bytes,
    ) -> JsonRpcResult<H256>;

    #[method(name = "shielded_getStatus", blocking)]
    fn shielded_get_status(&self) -> JsonRpcResult<ShieldedStatus>;

    // There is deliberately no `isNullifierSpent`: a wallet asking about
    // its own nullifiers would tell the node which notes are its own. A
    // wallet sees spends in the sections it scans (spec §7, Z-7).

    /// The executed shielded sections of up to `count` epochs from
    /// `from_epoch` on, for wallets to rebuild the tree and find notes.
    #[method(name = "shielded_getSections", blocking)]
    fn shielded_get_sections(
        &self, from_epoch: U64, count: U64,
    ) -> JsonRpcResult<Vec<ShieldedSection>>;

    /// The shielded state at a past epoch (while the node still has it):
    /// tree root and size, backing and turnstile counters, for auditing.
    #[method(name = "shielded_getStateAt", blocking)]
    fn shielded_get_state_at(&self, epoch: U64) -> JsonRpcResult<ShieldedStateAt>;

    /// Transaction `index` of a block's shielded section: its canonical body
    /// and, while the node still holds it, its proof. Public data, served
    /// one proof at a time (~380 KB each).
    #[method(name = "shielded_getWitness", blocking)]
    fn shielded_get_witness(
        &self, block_hash: H256, index: U64,
    ) -> JsonRpcResult<ShieldedWitnessInfo>;
}
