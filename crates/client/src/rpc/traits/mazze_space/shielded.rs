// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! The shielded space RPC (docs/internal/privacy-pq-stark.md §9.7.1). The
//! node serves public data only: it never sees a key, a note opening or a
//! Merkle path, and there is deliberately no `getTreePath`.

use crate::rpc::types::{
    shielded::{ShieldedSection, ShieldedStatus},
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

    /// Whether a 48-byte nullifier is spent in the latest executed state.
    #[method(name = "shielded_isNullifierSpent", blocking)]
    fn shielded_is_nullifier_spent(&self, nf: Bytes) -> JsonRpcResult<bool>;

    /// The executed shielded sections of up to `count` epochs from
    /// `from_epoch` on, for wallets to rebuild the tree and find notes.
    #[method(name = "shielded_getSections", blocking)]
    fn shielded_get_sections(
        &self, from_epoch: U64, count: U64,
    ) -> JsonRpcResult<Vec<ShieldedSection>>;
}
