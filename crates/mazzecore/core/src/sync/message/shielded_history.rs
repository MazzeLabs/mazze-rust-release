// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Requests for executed shielded sections (see `sync::shielded_history`).

use crate::{
    block_data_manager::ShieldedHistoryEntry,
    message::Message,
    sync::{
        message::{Context, Handleable},
        shielded_history::{serve, HistoryRow, MAX_HISTORY_EPOCHS_PER_REQUEST},
        Error,
    },
};
use rlp::{Decodable, DecoderError, Encodable, Rlp, RlpStream};
use rlp_derive::{RlpDecodable, RlpEncodable};

/// The sections of the epochs in `[from, to)`.
#[derive(Debug, Clone, PartialEq, RlpEncodable, RlpDecodable)]
pub struct GetShieldedHistory {
    pub from: u64,
    pub to: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShieldedHistory {
    pub from: u64,
    pub to: u64,
    /// Whether the sender's history is complete; if not, `entries` is
    /// empty and the requester asks someone else.
    pub complete: bool,
    pub entries: Vec<HistoryRow>,
}

impl Encodable for ShieldedHistory {
    fn rlp_append(&self, s: &mut RlpStream) {
        s.begin_list(4)
            .append(&self.from)
            .append(&self.to)
            .append(&self.complete);
        s.begin_list(self.entries.len());
        for (epoch, hash, entry) in &self.entries {
            s.begin_list(3).append(epoch).append(hash).append(entry);
        }
    }
}

impl Decodable for ShieldedHistory {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        if rlp.item_count()? != 4 {
            return Err(DecoderError::RlpIncorrectListLen);
        }
        let list = rlp.at(3)?;
        if list.item_count()? as u64 > MAX_HISTORY_EPOCHS_PER_REQUEST {
            return Err(DecoderError::Custom("Too many shielded sections"));
        }
        let mut entries = Vec::new();
        for item in list.iter() {
            if item.item_count()? != 3 {
                return Err(DecoderError::RlpIncorrectListLen);
            }
            let entry: ShieldedHistoryEntry = item.val_at(2)?;
            entries.push((item.val_at(0)?, item.val_at(1)?, entry));
        }
        Ok(ShieldedHistory {
            from: rlp.val_at(0)?,
            to: rlp.val_at(1)?,
            complete: rlp.val_at(2)?,
            entries,
        })
    }
}

impl Handleable for GetShieldedHistory {
    fn handle(self, ctx: &Context) -> Result<(), Error> {
        let response = serve(ctx.manager, self.from, self.to);
        if let Err(e) = response.send(ctx.io, &ctx.node_id) {
            debug!("sending shielded history failed: {:?}", e);
        }
        Ok(())
    }
}

impl Handleable for ShieldedHistory {
    fn handle(self, ctx: &Context) -> Result<(), Error> {
        ctx.manager
            .shielded_backfill
            .on_response(ctx.manager, ctx.io, &ctx.node_id, self);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mazze_executor::shielded_engine::ShieldedOutcome;
    use mazze_types::H256;
    use primitives::shielded::ShieldedTxBody;

    #[test]
    fn round_trip() {
        let entry = ShieldedHistoryEntry {
            bodies: vec![ShieldedTxBody::sample_for_tests(3)],
            outcomes: vec![ShieldedOutcome::Applied { positions: [0, 1] }],
        };
        let msg = ShieldedHistory {
            from: 10,
            to: 1010,
            complete: true,
            entries: vec![(20, H256::repeat_byte(1), entry)],
        };
        assert_eq!(rlp::decode::<ShieldedHistory>(&rlp::encode(&msg)).unwrap(), msg);
        let req = GetShieldedHistory { from: 1, to: 2 };
        assert_eq!(rlp::decode::<GetShieldedHistory>(&rlp::encode(&req)).unwrap(), req);
    }
}
