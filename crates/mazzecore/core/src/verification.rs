// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::{
    error::{BlockError, Error},
    pow::{self, PowComputer},
    sync::{Error as SyncError, ErrorKind as SyncErrorKind},
};
use mazze_executor::{machine::Machine, spec::TransitionsEpochHeight};
use mazze_parameters::{
    block::*, consensus_internal::ELASTICITY_MULTIPLIER, sync::MAX_PACKET_SIZE,
};
use mazze_storage::{
    into_simple_mpt_key, make_simple_mpt, simple_mpt_merkle_root,
    simple_mpt_proof, SimpleMpt, TrieProof,
};
use mazze_types::{
    address_util::AddressUtil, AllChainID, BigEndianHash, Space, SpaceMap,
    H256, U256,
};
use mazze_vm_types::Spec;
use primitives::{
    block::BlockHeight,
    block_header::compute_next_price_tuple,
    transaction::{
        native_transaction::TypedNativeTransaction, TransactionError,
    },
    shielded::ShieldedTxBody,
    Action, Block, BlockHeader, BlockReceipts, MerkleHash, Receipt,
    SignedTransaction, Transaction, TransactionWithSignature,
};
use keccak_hash::keccak;
use lazy_static::lazy_static;
use metrics::{register_meter_with_group, Meter};
use rlp::Encodable;
use rlp_derive::{RlpDecodable, RlpEncodable};
use serde_derive::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::{collections::HashSet, convert::TryInto, sync::Arc};
use unexpected::{Mismatch, OutOfBounds};

// PoW gate observability. Every `verify_pow()` outcome bumps exactly one
// of these counters. The ratio of bypassed vs verified is the canonical
// signal for "is this node actually validating proof-of-work?".
// See docs/chain-model.md §3 (PoW bypass table).
lazy_static! {
    static ref POW_VERIFIED_METER: Arc<dyn Meter> =
        register_meter_with_group("pow", "verification_verified");
    /// Bumped when the RandomX seed for this block's epoch is not yet
    /// known (early-epoch case). Should only fire in the first
    /// `RANDOMX_EPOCH_LENGTH` blocks of a fresh chain.
    static ref POW_SKIPPED_ZERO_SEED: Arc<dyn Meter> =
        register_meter_with_group(
            "pow",
            "verification_skipped_zero_seed_hash"
        );
    /// Bumped every time `set_catch_up_mode` flips the catch-up flag.
    /// Operators can graph this — any non-sync-phase-triggered flip is
    /// a tell. See docs/security-audit.md finding H-7.
    static ref CATCH_UP_TRANSITIONS: Arc<dyn Meter> =
        register_meter_with_group("pow", "catch_up_transitions_total");
}

/// External (sync graph) bypass counters. Defined here so all PoW
/// bypass metrics live in one namespace.
pub mod pow_metrics {
    use super::*;

    lazy_static! {
        /// Bumped when consortium mode short-circuits `verify_header_pow`.
        pub static ref SKIPPED_CONSORTIUM: Arc<dyn Meter> =
            register_meter_with_group(
                "pow",
                "verification_skipped_consortium"
            );
        /// Bumped when a locally-mined block in `bench_mode` fails PoW
        /// verification but is accepted anyway. A non-zero rate on a
        /// production node is a *real* anomaly — investigate.
        pub static ref BENCH_MODE_FAILURE: Arc<dyn Meter> =
            register_meter_with_group(
                "pow",
                "verification_bench_mode_failure"
            );
    }
}

#[derive(Clone)]
pub struct VerificationConfig {
    pub verify_timestamp: bool,
    pub referee_bound: usize,
    pub max_block_size_in_bytes: usize,
    pub transaction_epoch_bound: u64,
    pub max_nonce: Option<U256>,
    machine: Arc<Machine>,
    catch_up_mode: Arc<AtomicBool>,
}

/// Create an MPT from the ordered list of block transactions.
/// Keys are transaction indices, values are transaction hashes.
fn transaction_trie(transactions: &Vec<Arc<SignedTransaction>>) -> SimpleMpt {
    make_simple_mpt(
        transactions
            .iter()
            .map(|tx| tx.hash.as_bytes().into())
            .collect(),
    )
}

/// Compute block transaction root.
/// This value is stored in the `transactions_root` header field.
pub fn compute_transaction_root(
    transactions: &Vec<Arc<SignedTransaction>>,
) -> MerkleHash {
    simple_mpt_merkle_root(&mut transaction_trie(transactions))
}

fn shielded_trie(shielded: &[Arc<ShieldedTxBody>]) -> SimpleMpt {
    make_simple_mpt(
        shielded
            .iter()
            .map(|body| body.hash().as_bytes().into())
            .collect(),
    )
}

/// The `transactions_root` header field of a block. Without shielded
/// transactions it is the account transaction root, unchanged. With them it
/// also commits to the shielded section, whose bodies commit to their proofs.
pub fn compute_block_transactions_root(
    transactions: &Vec<Arc<SignedTransaction>>,
    shielded: &[Arc<ShieldedTxBody>],
) -> MerkleHash {
    let account_root = compute_transaction_root(transactions);
    if shielded.is_empty() {
        return account_root;
    }
    let shielded_root = simple_mpt_merkle_root(&mut shielded_trie(shielded));
    let mut preimage = Vec::with_capacity(SHIELDED_ROOT_DOMAIN.len() + 64);
    preimage.extend_from_slice(SHIELDED_ROOT_DOMAIN);
    preimage.extend_from_slice(account_root.as_bytes());
    preimage.extend_from_slice(shielded_root.as_bytes());
    keccak(&preimage)
}

const SHIELDED_ROOT_DOMAIN: &[u8] = b"mazze/transactions-root/shielded/v1";


/// Compute a proof for the `tx_index_in_block`-th transaction in a block.
pub fn compute_transaction_proof(
    transactions: &Vec<Arc<SignedTransaction>>, tx_index_in_block: usize,
) -> TrieProof {
    simple_mpt_proof(
        &mut transaction_trie(transactions),
        &into_simple_mpt_key(tx_index_in_block, transactions.len()),
    )
}

/// Create an MPT from the ordered list of block receipts.
/// Keys are receipt indices, values are RLP serialized receipts.
fn block_receipts_trie(block_receipts: &Vec<Receipt>) -> SimpleMpt {
    make_simple_mpt(
        block_receipts
            .iter()
            .map(|receipt| receipt.rlp_bytes().into_boxed_slice())
            .collect(),
    )
}

/// Compute block receipts root.
fn compute_block_receipts_root(block_receipts: &Vec<Receipt>) -> MerkleHash {
    simple_mpt_merkle_root(&mut block_receipts_trie(block_receipts))
}

/// Compute a proof for the `tx_index_in_block`-th receipt in a block.
pub fn compute_block_receipt_proof(
    block_receipts: &Vec<Receipt>, tx_index_in_block: usize,
) -> TrieProof {
    simple_mpt_proof(
        &mut block_receipts_trie(block_receipts),
        &into_simple_mpt_key(tx_index_in_block, block_receipts.len()),
    )
}

/// Create an MPT from the ordered list of epoch receipts.
/// Keys are block indices in the epoch, values are block receipts roots.
fn epoch_receipts_trie(epoch_receipts: &Vec<Arc<BlockReceipts>>) -> SimpleMpt {
    make_simple_mpt(
        epoch_receipts
            .iter()
            .map(|block_receipts| &block_receipts.receipts)
            .map(|rs| compute_block_receipts_root(&rs).as_bytes().into())
            .collect(),
    )
}

/// Compute epoch receipts root.
/// This value is stored in the `deferred_receipts_root` header field.
pub fn compute_receipts_root(
    epoch_receipts: &Vec<Arc<BlockReceipts>>,
) -> MerkleHash {
    simple_mpt_merkle_root(&mut epoch_receipts_trie(epoch_receipts))
}

#[derive(
    Clone,
    Debug,
    RlpEncodable,
    RlpDecodable,
    Default,
    PartialEq,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct EpochReceiptProof {
    pub block_index_proof: TrieProof,
    pub block_receipt_proof: TrieProof,
}

/// Compute a proof for the `tx_index_in_block`-th receipt
/// in the `block_index_in_epoch`-th block in an epoch.
pub fn compute_epoch_receipt_proof(
    epoch_receipts: &Vec<Arc<BlockReceipts>>, block_index_in_epoch: usize,
    tx_index_in_block: usize,
) -> EpochReceiptProof {
    let block_receipt_proof = compute_block_receipt_proof(
        &epoch_receipts[block_index_in_epoch].receipts,
        tx_index_in_block,
    );

    let block_index_proof = simple_mpt_proof(
        &mut epoch_receipts_trie(epoch_receipts),
        &into_simple_mpt_key(block_index_in_epoch, epoch_receipts.len()),
    );

    EpochReceiptProof {
        block_index_proof,
        block_receipt_proof,
    }
}

/// Use `proof` to verify that `tx_hash` is indeed the `tx_index_in_block`-th
/// transaction in a block with `num_txs_in_block` transactions and transaction
/// root `block_tx_root`.
pub fn is_valid_tx_inclusion_proof(
    block_tx_root: MerkleHash, tx_index_in_block: usize,
    num_txs_in_block: usize, tx_hash: H256, proof: &TrieProof,
) -> bool {
    let key = &into_simple_mpt_key(tx_index_in_block, num_txs_in_block);
    proof.is_valid_kv(key, Some(tx_hash.as_bytes()), &block_tx_root)
}

/// Use `block_index_proof` to get the correct block receipts trie root for the
/// `block_index_in_epoch`-th block in an epoch with `num_blocks_in_epoch`
/// blocks and receipts root `verified_epoch_receipts_root`.
/// Then, use `block_receipt_proof` to verify that `receipt` is indeed the
/// `tx_index_in_block`-th receipt in a block with `num_txs_in_block`
/// transactions and the transaction root from the previous step.
pub fn is_valid_receipt_inclusion_proof(
    verified_epoch_receipts_root: MerkleHash, block_index_in_epoch: usize,
    num_blocks_in_epoch: usize, block_index_proof: &TrieProof,
    tx_index_in_block: usize, num_txs_in_block: usize, receipt: &Receipt,
    block_receipt_proof: &TrieProof,
) -> bool {
    if block_index_in_epoch >= num_blocks_in_epoch
        || tx_index_in_block >= num_txs_in_block
    {
        return false;
    }

    // get block receipts root from block index trie (proof)
    // traversing along `key` also means we're validating the proof
    let key = &into_simple_mpt_key(block_index_in_epoch, num_blocks_in_epoch);

    let (is_valid_index_proof, maybe_val) =
        block_index_proof.get_value(key, &verified_epoch_receipts_root);
    if !is_valid_index_proof {
        return false;
    }
    let block_receipts_root_bytes = match maybe_val {
        Some(val) => val,
        None => return false,
    };

    // parse block receipts root as H256
    let block_receipts_root: H256 =
        match TryInto::<[u8; 32]>::try_into(block_receipts_root_bytes) {
            Ok(hash) => hash.into(),
            Err(_) => return false,
        };

    // validate receipt in the block receipts trie
    let key = &into_simple_mpt_key(tx_index_in_block, num_txs_in_block);

    block_receipt_proof.is_valid_kv(
        key,
        Some(&receipt.rlp_bytes()[..]),
        &block_receipts_root,
    )
}

/// How a block's shielded proofs are checked at acceptance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WitnessPolicy {
    /// Every proof present and verified: every block from the network that
    /// is not deeply buried.
    Require,
    /// A block buried under at least `WITNESS_BURIAL_ERAS` eras of valid
    /// headers may come without its witness, which every node has pruned
    /// by then (an archive rebuilding from genesis). Its bodies are still
    /// bound to the header; only the proofs are taken on checkpoint trust,
    /// as they were verified by the network when the block was new. A
    /// witness that is present is still verified.
    AllowPruned,
    /// A block re-read from this node's own database: verified when it was
    /// accepted.
    Skip,
}

/// Burial depth, in eras, from which a block may arrive without its
/// witness. Witnesses are kept until an era behind the stable checkpoint,
/// which itself trails the tip by about two eras, so any block shallower
/// than this still has its proofs on every honest peer.
pub const WITNESS_BURIAL_ERAS: u64 = 2;

impl VerificationConfig {
    pub fn new(
        test_mode: bool, referee_bound: usize, max_block_size_in_bytes: usize,
        transaction_epoch_bound: u64, tx_pool_nonce_bits: usize,
        machine: Arc<Machine>,
    ) -> Self {
        let max_block_size_in_bytes = if max_block_size_in_bytes
            > MAX_PACKET_SIZE
        {
            warn!(
                "max_block_size_in_bytes {} exceeds MAX_PACKET_SIZE {}, clamping",
                max_block_size_in_bytes, MAX_PACKET_SIZE
            );
            MAX_PACKET_SIZE
        } else {
            max_block_size_in_bytes
        };
        let max_nonce = if tx_pool_nonce_bits < 256 {
            Some((U256::one() << tx_pool_nonce_bits) - 1)
        } else {
            None
        };
        if test_mode {
            VerificationConfig {
                verify_timestamp: false,
                referee_bound,
                max_block_size_in_bytes,
                transaction_epoch_bound,
                machine,
                max_nonce,
                catch_up_mode: Arc::new(AtomicBool::new(false)),
            }
        } else {
            VerificationConfig {
                verify_timestamp: true,
                referee_bound,
                max_block_size_in_bytes,
                transaction_epoch_bound,
                machine,
                max_nonce,
                catch_up_mode: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    #[inline]
    /// Note that this function returns *pow_hash* of the block, not its quality
    pub fn get_or_fill_header_pow_hash(
        pow: &PowComputer, header: &mut BlockHeader, seed_hash: &H256,
    ) -> H256 {
        if header.pow_hash.is_none() {
            let computed_hash = Self::compute_pow_hash(pow, header, seed_hash);
            // trace, not info — archives verify ~2 nonces per
            // received header and this line was producing 6+ GB
            // log files on the fleet within a few hours (per
            // fleet incident 2026-07-08 report §5).
            trace!(
                "Computed PoW hash: {:?} for nonce: {:?}",
                computed_hash,
                header.nonce()
            );
            header.pow_hash = Some(computed_hash);
        }
        header.pow_hash.unwrap()
    }

    pub fn get_or_fill_header_pow_quality(
        pow: &PowComputer, header: &mut BlockHeader, seed_hash: &H256,
    ) -> U256 {
        let pow_hash =
            Self::get_or_fill_header_pow_hash(pow, header, seed_hash);
        pow::pow_hash_to_quality(&pow_hash, &header.nonce())
    }

    pub fn get_or_compute_header_pow_quality(
        pow: &PowComputer, header: &BlockHeader, seed_hash: &H256,
    ) -> U256 {
        let pow_hash = header
            .pow_hash
            .unwrap_or_else(|| Self::compute_pow_hash(pow, header, seed_hash));
        pow::pow_hash_to_quality(&pow_hash, &header.nonce())
    }

    fn compute_pow_hash(
        pow: &PowComputer, header: &BlockHeader, seed_hash: &H256,
    ) -> H256 {
        let nonce = header.nonce();
        let hash = header.problem_hash();

        pow.compute(&nonce, &hash, seed_hash)
    }

    #[inline]
    pub fn verify_pow(
        &self, pow: &PowComputer, header: &mut BlockHeader, seed_hash: &H256,
    ) -> Result<(), Error> {
        // Catch-up (bulk sync) no longer skips PoW: the headers come from
        // untrusted peers -- for a fast-sync node rooted at a trusted
        // checkpoint, PoW is what binds the header chain above it to the
        // network (docs/fast-sync-design.md §5.17/§5.19). The hash is needed
        // anyway: consensus classifies heavy/timer blocks by it, and it is
        // cached in the header (`get_or_fill_header_pow_hash`), so verifying
        // it here costs nothing extra.
        // PoW bypass B: zero seed hash. The RandomX seed for a block's
        // epoch lives at `block(RANDOMX_EPOCH_LENGTH * (N - 1))`. For
        // the first epoch no seed exists yet, so the validator can't
        // recompute the PoW hash — accept the block. See
        // docs/chain-model.md §3.
        if seed_hash.is_zero() {
            POW_SKIPPED_ZERO_SEED.mark(1);
            return Ok(());
        }

        let difficulty: U256 = header.difficulty().into();
        if difficulty.is_zero() {
            return Err(BlockError::InvalidDifficulty(OutOfBounds {
                min: Some(1.into()),
                max: None,
                found: 0.into(),
            })
            .into());
        }

        let hash = Self::get_or_fill_header_pow_hash(pow, header, seed_hash);
        let quality = pow::pow_hash_to_quality(&hash, &header.nonce());
        let boundary = pow::difficulty_to_boundary(&difficulty);

        if quality <= boundary {
            POW_VERIFIED_METER.mark(1);
            Ok(())
        } else {
            Err(From::from(BlockError::InvalidProofOfWork(OutOfBounds {
                min: Some(BigEndianHash::from_uint(&boundary)),
                max: None,
                found: BigEndianHash::from_uint(&quality),
            })))
        }
    }

    #[inline]
    pub fn validate_header_timestamp(
        &self, header: &BlockHeader, now: u64,
    ) -> Result<(), SyncError> {
        let invalid_threshold = now + VALID_TIME_DRIFT;
        if header.timestamp() > invalid_threshold {
            warn!("block {} has incorrect timestamp", header.hash());
            return Err(SyncErrorKind::InvalidTimestamp.into());
        }
        Ok(())
    }

    /// Check basic header parameters.
    /// This does not require header to be graph or parental tree ready.
    #[inline]
    pub fn verify_header_params(
        &self, pow: &PowComputer, header: &mut BlockHeader, seed_hash: &H256,
    ) -> Result<(), Error> {
        // Check header custom data length
        let custom_len = header.custom().iter().fold(0, |acc, x| acc + x.len());
        if custom_len > HEADER_CUSTOM_LENGTH_BOUND {
            return Err(From::from(BlockError::TooLongCustomInHeader(
                OutOfBounds {
                    min: Some(0),
                    max: Some(HEADER_CUSTOM_LENGTH_BOUND),
                    found: custom_len,
                },
            )));
        }

        if header.base_price().is_none() {
            bail!(BlockError::MissingBaseFee);
        }

        // Note that this is just used to rule out deprecated blocks, so the
        // change of header struct actually happens before the change of
        // reward is reflected in the state root. The first state root
        // including results of new rewards will in the header after another
        // REWARD_EPOCH_COUNT + DEFERRED_STATE_EPOCH_COUNT epochs.
        if let Some(expected_custom_prefix) =
            self.machine.params().custom_prefix(header.height())
        {
            for (i, expected_bytes) in expected_custom_prefix.iter().enumerate()
            {
                let header_custum = header.custom();
                // Header custom is too short.
                let b =
                    header_custum.get(i).ok_or(BlockError::InvalidCustom(
                        header_custum.clone(),
                        expected_custom_prefix.clone(),
                    ))?;
                if b != expected_bytes {
                    return Err(BlockError::InvalidCustom(
                        header_custum.clone(),
                        expected_custom_prefix.clone(),
                    )
                    .into());
                }
            }
        }

        // verify POW
        self.verify_pow(pow, header, seed_hash)?;

        // A block will be invalid if it has more than REFEREE_BOUND referees
        if header.referee_hashes().len() > self.referee_bound {
            return Err(From::from(BlockError::TooManyReferees(OutOfBounds {
                min: Some(0),
                max: Some(self.referee_bound),
                found: header.referee_hashes().len(),
            })));
        }

        // verify non-duplicated parent and referee hashes
        let mut direct_ancestor_hashes = HashSet::new();
        let parent_hash = header.parent_hash();
        direct_ancestor_hashes.insert(parent_hash.clone());
        for referee_hash in header.referee_hashes() {
            if direct_ancestor_hashes.contains(referee_hash) {
                warn!(
                    "block {} has duplicate parent or referee hashes",
                    header.hash()
                );
                return Err(From::from(
                    BlockError::DuplicateParentOrRefereeHashes(
                        referee_hash.clone(),
                    ),
                ));
            }
            direct_ancestor_hashes.insert(referee_hash.clone());
        }

        Ok(())
    }

    /// Verify block data against header: transactions root
    #[inline]
    fn verify_block_integrity(&self, block: &Block) -> Result<(), Error> {
        let expected_root = compute_block_transactions_root(
            &block.transactions,
            &block.shielded,
        );
        if &expected_root != block.block_header.transactions_root() {
            warn!("Invalid transaction root");
            bail!(BlockError::InvalidTransactionsRoot(Mismatch {
                expected: expected_root,
                found: *block.block_header.transactions_root(),
            }));
        }
        Ok(())
    }

    /// Phase 1 quick block verification. Only does checks that are cheap.
    /// Operates on a single block.
    /// Note that we should first check whether the block body matches its
    /// header, e.g., check transaction root correctness, and then check the
    /// body itself. If body does not match header, the block may still be
    /// valid but we get the wrong body from evil node. So we try to get
    /// body again from others. However, if the body matches the header and
    /// the body is incorrect, this means the block is invalid, and we
    /// should discard this block and all its descendants.
    #[inline]
    ///
    /// `witness` says how the shielded proofs are checked (see
    /// `WitnessPolicy`).
    pub fn verify_sync_graph_block_basic(
        &self, block: &Block, chain_id: AllChainID, witness: WitnessPolicy,
    ) -> Result<(), Error> {
        self.verify_block_integrity(block)?;
        self.verify_shielded_section(block, chain_id, witness)?;

        let block_height = block.block_header.height();

        let mut block_size = 0;
        let transitions = &self.machine.params().transition_heights;

        for t in &block.transactions {
            self.verify_transaction_common(
                t,
                chain_id,
                block_height,
                transitions,
                VerifyTxMode::Remote,
            )?;
            block_size += t.rlp_size();
        }

        if block_size > self.max_block_size_in_bytes {
            return Err(From::from(BlockError::InvalidBlockSize(
                OutOfBounds {
                    min: None,
                    max: Some(self.max_block_size_in_bytes as u64),
                    found: block_size as u64,
                },
            )));
        }
        Ok(())
    }

    /// Stage A for the shielded section (spec §8.2): the slot and count
    /// rules, the constant fee, distinct nullifiers, and every proof. A
    /// proof that does not match its body's `proof_hash` came from the peer,
    /// not the miner, so the block is requested again; anything else makes
    /// the block invalid.
    /// Stage A for a block that needs no other check (one this node mined):
    /// its shielded section and every proof.
    pub fn verify_shielded_proofs(
        &self, block: &Block, chain_id: AllChainID,
    ) -> Result<(), Error> {
        self.verify_shielded_section(block, chain_id, WitnessPolicy::Require)
    }

    fn verify_shielded_section(
        &self, block: &Block, chain_id: AllChainID, witness: WitnessPolicy,
    ) -> Result<(), Error> {
        if block.shielded.is_empty() {
            return Ok(());
        }
        let params = &self.machine.params().shielded;
        let height = block.block_header.height();
        if !params.is_slot(height) {
            bail!(BlockError::ShieldedSectionOutsideSlot(height));
        }
        if block.shielded.len() > params.max_txs_per_block {
            bail!(BlockError::TooManyShieldedTransactions(OutOfBounds {
                min: None,
                max: Some(params.max_txs_per_block),
                found: block.shielded.len(),
            }));
        }
        let mut nullifiers = HashSet::new();
        for body in &block.shielded {
            if body.fee != params.tx_fee {
                bail!(BlockError::InvalidShieldedTransaction(format!(
                    "fee {} instead of {}",
                    body.fee, params.tx_fee
                )));
            }
            for nf in &body.nf {
                if !nullifiers.insert(*nf) {
                    bail!(BlockError::InvalidShieldedTransaction(
                        "nullifier spent twice in one section".into()
                    ));
                }
            }
        }
        match witness {
            WitnessPolicy::Skip => return Ok(()),
            // Pruned everywhere: only the absence of every proof qualifies,
            // never a partial witness.
            WitnessPolicy::AllowPruned if block.witness.is_empty() => {
                debug!(
                    "Accepting buried block {:?} whose witness was pruned",
                    block.hash()
                );
                return Ok(());
            }
            _ => {}
        }
        if !block.has_complete_witness() {
            bail!(BlockError::ShieldedWitnessMismatch(
                "missing proofs".into()
            ));
        }
        // Every proof must hash to its body before any STARK runs: a peer
        // that sends one wrong proof among eight costs us a hash, not seven
        // verifications.
        for (body, proof) in block.shielded.iter().zip(&block.witness) {
            if primitives::shielded::proof_hash(proof) != body.proof_hash {
                bail!(BlockError::ShieldedWitnessMismatch(format!(
                    "proof does not match body {:?}",
                    body.hash()
                )));
            }
        }
        // The STARKs in parallel: ~7 ms each, up to `max_txs_per_block` of
        // them. The first failure in block order is reported, so the outcome
        // does not depend on scheduling. Every proof already hashes to its
        // body, so a failure here is the block's fault.
        use mazze_shielded_pq::verify::verify_body;
        use rayon::prelude::*;
        let shielded_chain_id = chain_id.in_shielded_space();
        let results: Vec<_> = block
            .shielded
            .par_iter()
            .zip(block.witness.par_iter())
            .map(|(body, proof)| verify_body(body, proof, shielded_chain_id))
            .collect();
        for (body, result) in block.shielded.iter().zip(results) {
            if let Err(e) = result {
                bail!(BlockError::InvalidShieldedTransaction(format!(
                    "{:?}: {:?}",
                    body.hash(),
                    e
                )));
            }
        }
        Ok(())
    }

    pub fn verify_sync_graph_ready_block(
        &self, block: &Block, parent: &BlockHeader,
    ) -> Result<(), Error> {
        let mut total_gas: SpaceMap<U256> = SpaceMap::default();
        for t in &block.transactions {
            total_gas[t.space()] += *t.gas_limit();
        }

        self.check_base_fee(block, parent, total_gas)?;

        Ok(())
    }

    fn check_base_fee(
        &self, block: &Block, parent: &BlockHeader, total_gas: SpaceMap<U256>,
    ) -> Result<(), Error> {
        use Space::*;

        let params = self.machine.params();
        let block_height = block.block_header.height();

        let core_gas_limit = block.block_header.gas_limit() * 9 / 10;
        let espace_gas_limit =
            if self.machine.params().can_pack_evm_transaction(block_height) {
                block.block_header.gas_limit() * 5 / 10
            } else {
                U256::zero()
            };

        if total_gas[Ethereum] > espace_gas_limit {
            return Err(From::from(BlockError::InvalidPackedGasLimit(
                OutOfBounds {
                    min: None,
                    max: Some(espace_gas_limit),
                    found: total_gas[Ethereum],
                },
            )));
        }

        // The shielded lane has no gas budget yet.
        let shielded_gas_limit = U256::zero();
        if total_gas[Shielded] > shielded_gas_limit {
            return Err(From::from(BlockError::InvalidPackedGasLimit(
                OutOfBounds {
                    min: None,
                    max: Some(shielded_gas_limit),
                    found: total_gas[Shielded],
                },
            )));
        }

        if total_gas[Native] > core_gas_limit {
            return Err(From::from(BlockError::InvalidPackedGasLimit(
                OutOfBounds {
                    min: None,
                    max: Some(core_gas_limit),
                    found: total_gas[Native],
                },
            )));
        }

        let parent_base_price = if block_height == 1 {
            params.init_base_price()
        } else {
            parent.base_price().unwrap()
        };

        let gas_limit =
            SpaceMap::new(core_gas_limit, espace_gas_limit, shielded_gas_limit);
        let gas_target = gas_limit.map_all(|x| x / ELASTICITY_MULTIPLIER);
        let min_base_price = params.min_base_price();

        let expected_base_price = SpaceMap::zip4(
            gas_target,
            total_gas,
            parent_base_price,
            min_base_price,
        )
        .map_all(compute_next_price_tuple);

        let actual_base_price = block.block_header.base_price().unwrap();

        if actual_base_price != expected_base_price {
            return Err(From::from(BlockError::InvalidBasePrice(Mismatch {
                expected: expected_base_price,
                found: actual_base_price,
            })));
        }

        Ok(())
    }

    pub fn check_transaction_epoch_bound(
        tx: &TypedNativeTransaction, block_height: u64,
        transaction_epoch_bound: u64,
    ) -> i8 {
        if tx.epoch_height().wrapping_add(transaction_epoch_bound)
            < block_height
        {
            -1
        } else if *tx.epoch_height() > block_height + transaction_epoch_bound {
            1
        } else {
            0
        }
    }

    fn verify_transaction_epoch_height(
        tx: &TypedNativeTransaction, block_height: u64,
        transaction_epoch_bound: u64, mode: &VerifyTxMode,
    ) -> Result<(), TransactionError> {
        let result = Self::check_transaction_epoch_bound(
            tx,
            block_height,
            transaction_epoch_bound,
        );
        let allow_larger_epoch = mode.is_maybe_later();

        if result == 0 || (result > 0 && allow_larger_epoch) {
            Ok(())
        } else {
            bail!(TransactionError::EpochHeightOutOfBound {
                set: *tx.epoch_height(),
                block_height,
                transaction_epoch_bound,
            });
        }
    }

    fn fast_recheck_inner<F>(spec: &Spec, f: F) -> (bool, bool)
    where
        F: Fn(&VerifyTxMode) -> bool,
    {
        let tx_pool_mode =
            VerifyTxMode::Local(VerifyTxLocalMode::MaybeLater, spec);
        let packing_mode = VerifyTxMode::Local(VerifyTxLocalMode::Full, spec);

        (f(&packing_mode), f(&tx_pool_mode))
    }

    pub fn fast_recheck(
        &self, tx: &TransactionWithSignature, height: BlockHeight,
        _transitions: &TransitionsEpochHeight, spec: &Spec,
    ) -> PackingCheckResult {
        let (can_pack, later_pack) =
            Self::fast_recheck_inner(spec, |mode: &VerifyTxMode| {
                if !Self::check_eip1559_transaction(tx, mode) {
                    return false;
                }

                if let Transaction::Native(ref tx) = tx.unsigned {
                    Self::verify_transaction_epoch_height(
                        tx,
                        height,
                        self.transaction_epoch_bound,
                        mode,
                    )
                    .is_ok()
                } else {
                    Self::check_eip155_transaction(tx, mode)
                }
            });

        match (can_pack, later_pack) {
            (true, _) => PackingCheckResult::Pack,
            (false, true) => PackingCheckResult::Pending,
            (false, false) => PackingCheckResult::Drop,
        }
    }

    // Packing transactions, verifying transaction in sync graph and inserting
    // transactions may have different logics. But they share a lot of similar
    // rules. We combine them together for convenient in the future upgrades..
    pub fn verify_transaction_common(
        &self, tx: &TransactionWithSignature, chain_id: AllChainID,
        height: BlockHeight, _transitions: &TransitionsEpochHeight,
        mode: VerifyTxMode,
    ) -> Result<(), TransactionError> {
        tx.check_low_s()?;
        tx.check_y_parity()?;

        // Disallow unsigned transactions
        if tx.is_unsigned() {
            bail!(TransactionError::InvalidSignature(
                "Transaction is unsigned".into()
            ));
        }

        if let Some(tx_chain_id) = tx.chain_id() {
            if tx_chain_id != chain_id.in_space(tx.space()) {
                bail!(TransactionError::ChainIdMismatch {
                    expected: chain_id.in_space(tx.space()),
                    got: tx_chain_id,
                    space: tx.space(),
                });
            }
        }

        // Forbid zero-gas-price tx
        if tx.gas_price().is_zero() {
            bail!(TransactionError::ZeroGasPrice);
        }

        if matches!(mode, VerifyTxMode::Local(..))
            && tx.space() == Space::Native
        {
            if let Action::Call(ref address) = tx.transaction.action() {
                if !address.is_genesis_valid_address() {
                    bail!(TransactionError::InvalidReceiver)
                }
            }
        }

        if let (VerifyTxMode::Local(..), Some(max_nonce)) =
            (mode, self.max_nonce)
        {
            if tx.nonce() > &max_nonce {
                bail!(TransactionError::TooLargeNonce)
            }
        }

        // ******************************************
        // Each constraint depends on a mode or a MIP should be
        // implemented in a separated function.
        // ******************************************

        if let Transaction::Native(ref tx) = tx.unsigned {
            Self::verify_transaction_epoch_height(
                tx,
                height,
                self.transaction_epoch_bound,
                &mode,
            )?;
        }

        if !Self::check_eip155_transaction(tx, &mode) {
            bail!(TransactionError::FutureTransactionType);
        }

        if !Self::check_eip1559_transaction(tx, &mode) {
            bail!(TransactionError::FutureTransactionType)
        }

        Self::check_gas_limit_with_calldata(tx)?;
        Ok(())
    }

    fn check_eip155_transaction(
        _tx: &TransactionWithSignature, _mode: &VerifyTxMode,
    ) -> bool {
        return true;
    }

    fn check_eip1559_transaction(
        _tx: &TransactionWithSignature, _mode: &VerifyTxMode,
    ) -> bool {
        return true;
    }

    fn check_gas_limit_with_calldata(
        tx: &TransactionWithSignature,
    ) -> Result<(), TransactionError> {
        let data_length = tx.data().len();
        let min_gas_limit = data_length.saturating_mul(100);
        if tx.gas() < &U256::from(min_gas_limit) {
            bail!(TransactionError::NotEnoughBaseGas {
                required: min_gas_limit.into(),
                got: *tx.gas()
            });
        }
        Ok(())
    }

    pub fn check_tx_size(
        &self, tx: &TransactionWithSignature,
    ) -> Result<(), TransactionError> {
        if tx.rlp_size() > self.max_block_size_in_bytes {
            bail!(TransactionError::TooBig)
        } else {
            Ok(())
        }
    }

    pub fn catch_up_mode(&self) -> bool {
        self.catch_up_mode.load(AtomicOrdering::SeqCst)
    }

    /// Set the catch-up flag. **Only one legitimate caller exists**:
    /// [`SynchronizationProtocolHandler::update_sync_phase`] in
    /// `crates/mazzecore/core/src/sync/synchronization_protocol_handler.rs`.
    /// That setter mirrors the sync-phase manager's view of the
    /// catch-up state so [`Self::verify_pow`] can short-circuit PoW
    /// checks during bulk header sync (see docs/chain-model.md §3
    /// bypass table).
    ///
    /// **DO NOT** call from RPC handlers, peer message handlers, or
    /// any other code path. A `true` here disables PoW verification
    /// for every subsequent block until restored to `false`. Every
    /// transition is metered as `pow.catch_up_transitions_total` so
    /// operators can spot unexpected flips. See
    /// docs/security-audit.md finding H-7.
    pub fn set_catch_up_mode(&self, mode: bool) {
        let prev = self.catch_up_mode.swap(mode, AtomicOrdering::SeqCst);
        if prev != mode {
            CATCH_UP_TRANSITIONS.mark(1);
            log::info!(
                "verify_pow catch_up_mode transition: {} -> {}",
                prev, mode
            );
        }
    }
}

#[derive(Copy, Clone)]
pub enum PackingCheckResult {
    Pack,
    // Transaction can be packed.
    Pending,
    // Transaction may be ready to packed in the future.
    Drop, // Transaction can never be packed.
}

#[derive(Copy, Clone)]
pub enum VerifyTxMode<'a> {
    /// Check transactions in local mode, may have more constraints
    Local(VerifyTxLocalMode, &'a Spec),
    /// Check transactions for received blocks in sync graph, may have less
    /// constraints
    Remote,
}

#[derive(Copy, Clone)]
pub enum VerifyTxLocalMode {
    /// Apply all checks
    Full,
    /// If a transaction is not valid now, but can become valid in the future,
    /// the check sould pass
    MaybeLater,
}

impl<'a> VerifyTxMode<'a> {
    fn is_maybe_later(&self) -> bool {
        if let VerifyTxMode::Local(VerifyTxLocalMode::MaybeLater, _) = self {
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod shielded_root_tests {
    use super::{compute_block_transactions_root, compute_transaction_root, WitnessPolicy};
    use primitives::shielded::ShieldedTxBody;
    use std::sync::Arc;

    fn bodies(seeds: &[u32]) -> Vec<Arc<ShieldedTxBody>> {
        seeds
            .iter()
            .map(|s| Arc::new(ShieldedTxBody::sample_for_tests(*s)))
            .collect()
    }

    #[test]
    fn root_is_unchanged_without_shielded_transactions() {
        assert_eq!(
            compute_block_transactions_root(&vec![], &[]),
            compute_transaction_root(&vec![])
        );
    }

    #[test]
    fn root_commits_to_the_shielded_section() {
        let plain = compute_block_transactions_root(&vec![], &[]);
        let one = compute_block_transactions_root(&vec![], &bodies(&[1]));
        let two = compute_block_transactions_root(&vec![], &bodies(&[1, 2]));
        let swapped =
            compute_block_transactions_root(&vec![], &bodies(&[2, 1]));
        assert_ne!(one, plain);
        assert_ne!(two, one);
        assert_ne!(swapped, two, "order is committed");

        // A different proof is a different body, so a different root.
        let mut other = ShieldedTxBody::sample_for_tests(1);
        other.proof_hash.0[0] ^= 1;
        let other_root =
            compute_block_transactions_root(&vec![], &[Arc::new(other)]);
        assert_ne!(other_root, one);
    }

    fn config(params: mazze_executor::spec::ShieldedParams) -> super::VerificationConfig {
        use mazze_executor::{
            machine::{new_machine_with_builtin, VmFactory},
            spec::CommonParams,
        };
        use mazze_parameters::{
            block::{MAX_BLOCK_SIZE_IN_BYTES, REFEREE_DEFAULT_BOUND},
            consensus::TRANSACTION_DEFAULT_EPOCH_BOUND,
            tx_pool::TXPOOL_DEFAULT_NONCE_BITS,
        };
        let mut common = CommonParams::default();
        common.shielded = params;
        super::VerificationConfig::new(
            true,
            REFEREE_DEFAULT_BOUND,
            MAX_BLOCK_SIZE_IN_BYTES,
            TRANSACTION_DEFAULT_EPOCH_BOUND,
            TXPOOL_DEFAULT_NONCE_BITS,
            Arc::new(new_machine_with_builtin(common, VmFactory::default())),
        )
    }

    fn block_at(
        height: u64, shielded: Vec<Arc<ShieldedTxBody>>,
        witness: Vec<Arc<Vec<u8>>>,
    ) -> primitives::Block {
        let header = primitives::BlockHeaderBuilder::new()
            .with_height(height)
            .with_transactions_root(compute_block_transactions_root(
                &vec![],
                &shielded,
            ))
            .build();
        primitives::Block::new(header, vec![])
            .with_shielded(shielded)
            .with_witness(witness)
    }

    fn kind(r: Result<(), crate::error::Error>) -> String {
        match r {
            Ok(()) => "ok".into(),
            Err(e) => match e.kind() {
                crate::error::ErrorKind::Block(b) => format!("{b:?}")
                    .split('(')
                    .next()
                    .unwrap()
                    .to_string(),
                other => format!("{other:?}"),
            },
        }
    }

    /// Stage A at block acceptance, with one real transaction.
    #[test]
    fn stage_a_accepts_valid_sections_and_classifies_failures() {
        use mazze_executor::spec::ShieldedParams;
        use mazze_types::AllChainID;

        let chain = AllChainID::new(1, 1, 1);
        let active = ShieldedParams {
            activation_height: 0,
            slot_epochs: 1,
            ..ShieldedParams::default()
        };
        let fee = active.tx_fee;
        let (body, proof) =
            mazze_shielded_pq::testkit::payment(1, 100 * fee, fee, 1);
        let body = Arc::new(body);
        let proof = Arc::new(proof);
        let cfg = config(active.clone());
        let check = |cfg: &super::VerificationConfig,
                     b: &primitives::Block,
                     witness: bool| {
            let policy = if witness { WitnessPolicy::Require } else { WitnessPolicy::Skip };
            kind(cfg.verify_sync_graph_block_basic(b, chain, policy))
        };

        let good = block_at(5, vec![body.clone()], vec![proof.clone()]);
        assert_eq!(check(&cfg, &good, true), "ok");
        // Re-read from the node's own database: no witness, not re-proved.
        let from_db = block_at(5, vec![body.clone()], vec![]);
        assert_eq!(check(&cfg, &from_db, false), "ok");

        // The peer's fault: the block is requested again, not condemned.
        assert_eq!(check(&cfg, &from_db, true), "ShieldedWitnessMismatch");
        // Buried deep enough, a pruned witness is accepted; a partial or a
        // wrong one never is.
        let buried = |b: &primitives::Block| {
            kind(cfg.verify_sync_graph_block_basic(b, chain, WitnessPolicy::AllowPruned))
        };
        assert_eq!(buried(&from_db), "ok");
        assert_eq!(buried(&good), "ok");
        let partial = block_at(5, vec![body.clone()], vec![proof.clone(), proof.clone()]);
        assert_eq!(buried(&partial), "ShieldedWitnessMismatch");
        let wrong = block_at(5, vec![body.clone()], vec![Arc::new(vec![7u8; 1000])]);
        assert_eq!(check(&cfg, &wrong, true), "ShieldedWitnessMismatch");

        // The miner's fault: the block is invalid.
        let twice = block_at(
            5,
            vec![body.clone(), body.clone()],
            vec![proof.clone(), proof.clone()],
        );
        assert_eq!(check(&cfg, &twice, true), "InvalidShieldedTransaction");
        let other_fee = config(ShieldedParams { tx_fee: fee + 1, ..active.clone() });
        assert_eq!(check(&other_fee, &good, true), "InvalidShieldedTransaction");
        let other_chain = AllChainID::new(1, 1, 2);
        assert_eq!(
            kind(cfg.verify_sync_graph_block_basic(&good, other_chain, WitnessPolicy::Require)),
            "InvalidShieldedTransaction"
        );

        // Slots and activation.
        let every_second = config(ShieldedParams { slot_epochs: 2, ..active.clone() });
        assert_eq!(check(&every_second, &good, true), "ShieldedSectionOutsideSlot");
        let inactive = config(ShieldedParams::default());
        assert_eq!(check(&inactive, &good, true), "ShieldedSectionOutsideSlot");
        // Paused at or before the block's height: no section is valid.
        let paused = config(ShieldedParams { pause_height: 5, ..active.clone() });
        assert_eq!(check(&paused, &good, true), "ShieldedSectionOutsideSlot");
        let paused_later = config(ShieldedParams { pause_height: 6, ..active.clone() });
        assert_eq!(check(&paused_later, &good, true), "ok");
        let one_per_block = config(ShieldedParams { max_txs_per_block: 0, ..active });
        assert_eq!(check(&one_per_block, &good, true), "TooManyShieldedTransactions");

        // A block without a section is unaffected by any of this.
        assert_eq!(check(&inactive, &block_at(5, vec![], vec![]), true), "ok");
    }
}

#[cfg(test)]
mod tests {
    use crate::verification::EpochReceiptProof;
    use mazze_storage::{
        CompressedPathRaw, TrieProof, TrieProofNode, VanillaChildrenTable,
    };

    #[test]
    fn test_rlp_epoch_receipt_proof() {
        let proof = EpochReceiptProof::default();
        assert_eq!(proof, rlp::decode(&rlp::encode(&proof)).unwrap());

        let serialized = serde_json::to_string(&proof).unwrap();
        let deserialized: EpochReceiptProof =
            serde_json::from_str(&serialized).unwrap();
        assert_eq!(proof, deserialized);

        let node1 = TrieProofNode::new(
            Default::default(),
            Some(Box::new([0x03, 0x04, 0x05])),
            CompressedPathRaw::new(
                &[0x00, 0x01, 0x02],
                CompressedPathRaw::first_nibble_mask(),
            ),
            /* path_without_first_nibble = */ true,
        );

        let root_node = {
            let mut children_table = VanillaChildrenTable::default();
            unsafe {
                *children_table.get_child_mut_unchecked(2) =
                    *node1.get_merkle();
                *children_table.get_children_count_mut() = 1;
            }
            TrieProofNode::new(
                children_table,
                None,
                CompressedPathRaw::default(),
                /* path_without_first_nibble = */ false,
            )
        };
        let nodes = [root_node, node1]
            .iter()
            .cloned()
            .cycle()
            .take(20)
            .collect();
        let proof = TrieProof::new(nodes).unwrap();

        let epoch_proof = EpochReceiptProof {
            block_index_proof: proof.clone(),
            block_receipt_proof: proof,
        };

        assert_eq!(
            epoch_proof,
            rlp::decode(&rlp::encode(&epoch_proof)).unwrap()
        );

        let serialized = serde_json::to_string(&epoch_proof).unwrap();
        let deserialized: EpochReceiptProof =
            serde_json::from_str(&serialized).unwrap();
        assert_eq!(epoch_proof, deserialized);
    }
}
