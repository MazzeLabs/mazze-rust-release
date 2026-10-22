// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Relay of shielded transactions: each one is its body and its proof. The
//! `stem` flag is the Dandelion++ phase (see `sync::dandelion`).

use crate::sync::{
    message::{Context, Handleable},
    Error, ErrorKind,
};
use primitives::shielded::{ShieldedTxBody, MAX_PROOF_BYTES};
use rlp::{Decodable, DecoderError, Encodable, Rlp, RlpStream};

/// Transactions in one message: proofs are large.
pub const MAX_SHIELDED_TXS_PER_MESSAGE: usize = 8;

#[derive(Debug, Clone, PartialEq)]
pub struct ShieldedTransactions {
    pub stem: bool,
    pub txs: Vec<(ShieldedTxBody, Vec<u8>)>,
}

impl Encodable for ShieldedTransactions {
    fn rlp_append(&self, s: &mut RlpStream) {
        s.begin_list(2).append(&self.stem);
        s.begin_list(self.txs.len());
        for (body, proof) in &self.txs {
            s.begin_list(2).append(body).append(&proof.as_slice());
        }
    }
}

impl Decodable for ShieldedTransactions {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        if rlp.item_count()? != 2 {
            return Err(DecoderError::RlpIncorrectListLen);
        }
        let list = rlp.at(1)?;
        if list.item_count()? > MAX_SHIELDED_TXS_PER_MESSAGE {
            return Err(DecoderError::Custom("Too many shielded transactions"));
        }
        let mut txs = Vec::new();
        for item in list.iter() {
            if item.item_count()? != 2 {
                return Err(DecoderError::RlpIncorrectListLen);
            }
            let proof = item.at(1)?.data()?;
            if proof.len() > MAX_PROOF_BYTES {
                return Err(DecoderError::Custom("Proof too long"));
            }
            txs.push((item.val_at(0)?, proof.to_vec()));
        }
        Ok(ShieldedTransactions {
            stem: rlp.val_at(0)?,
            txs,
        })
    }
}

impl Handleable for ShieldedTransactions {
    fn handle(self, ctx: &Context) -> Result<(), Error> {
        // A broadcast needs the executed state, which is not current while
        // catching up. A stem does not: its proof is checked statelessly and
        // its state leniently, and it goes on, since a stem dropped here
        // would leave its origin to fluff it.
        if ctx.manager.catch_up_mode() && !self.stem {
            return Ok(());
        }
        let pool = ctx.manager.graph.consensus.get_tx_pool();
        for (body, proof) in self.txs {
            let hash = body.hash();
            let known = pool.shielded.contains(&hash);
            if !known {
                use crate::transaction_pool::shielded::ShieldedSource;
                let source = if self.stem {
                    ShieldedSource::PeerStem
                } else {
                    ShieldedSource::PeerFluff
                };
                match pool.insert_shielded(body, proof, source) {
                    Ok(_) => {}
                    // Validity of a proof does not depend on state: an
                    // honest peer never relays an invalid one, and each
                    // costs a STARK. The sender is disconnected.
                    Err(crate::transaction_pool::shielded::ShieldedPoolError::InvalidProof(e)) => {
                        debug!("shielded transaction {:?} with an invalid proof: {}", hash, e);
                        return Err(ErrorKind::InvalidMessageFormat.into());
                    }
                    Err(e) => {
                        debug!("shielded transaction {:?} refused: {:?}", hash, e);
                        continue;
                    }
                }
            }
            if !self.stem {
                pool.shielded.mark_fluffed(&hash);
            }
            let route = if self.stem {
                ctx.manager.dandelion.route_stem(hash, &ctx.node_id, &ctx.manager.outbound_peer_ids(ctx.io))
            } else {
                ctx.manager.dandelion.route_fluff(hash)
            };
            ctx.manager.relay_shielded(ctx.io, hash, route, Some(&ctx.node_id));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_limits() {
        let msg = ShieldedTransactions {
            stem: true,
            txs: vec![(ShieldedTxBody::sample_for_tests(1), vec![1, 2, 3])],
        };
        let decoded: ShieldedTransactions = rlp::decode(&rlp::encode(&msg)).unwrap();
        assert_eq!(decoded, msg);

        let many = ShieldedTransactions {
            stem: false,
            txs: (0..9).map(|i| (ShieldedTxBody::sample_for_tests(i), vec![])).collect(),
        };
        assert!(rlp::decode::<ShieldedTransactions>(&rlp::encode(&many)).is_err());

        let huge = ShieldedTransactions {
            stem: false,
            txs: vec![(ShieldedTxBody::sample_for_tests(1), vec![0; MAX_PROOF_BYTES + 1])],
        };
        assert!(rlp::decode::<ShieldedTransactions>(&rlp::encode(&huge)).is_err());
    }
}
