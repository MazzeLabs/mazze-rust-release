// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::{
    shielded::{ShieldedTxBody, MAX_PROOF_BYTES}, BlockHeader, SignedTransaction,
    TransactionWithSignature,
};
use byteorder::{ByteOrder, LittleEndian};
use keccak_hash::keccak;
use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};
use mazze_types::{Space, H256, U256};
use rand::Rng;
use rlp::{Decodable, DecoderError, Encodable, Rlp, RlpStream};
use siphasher::sip::SipHasher24;
use std::{
    fmt::{Debug, Formatter},
    hash::Hasher,
    sync::Arc,
};

/// Marks a stored body that has a shielded section (see
/// `Block::encode_stored_body`).
const STORED_BODY_WITH_SHIELDED: u8 = 0x01;

/// Ethereum-compatible linear block index. Computed as
/// `epoch_start_block_number + block_index_in_epoch` (see
/// [`crate::block_number::compute_block_number`]). This is what
/// `eth_blockNumber` returns and what user-facing RPCs expose. **It is
/// not** the same as `BlockHeight` — that's a header-metadata field
/// recording the producer's DAG-local distance from genesis. Prefer
/// `BlockNumber` when you mean "where does this block sit in the linear
/// order an Ethereum client would expect".
///
/// See `docs/chain-model.md` §1 for the full glossary of chain-height
/// terms.
pub type BlockNumber = u64;

/// DAG-local distance from genesis as recorded by the block's producer
/// in its header. **Not** a consensus sort key — many blocks can share
/// the same `BlockHeight` because Mazze is a DAG. Use `EpochNumber` for
/// consensus ordering and `BlockNumber` for the Ethereum-compatible
/// linear index.
///
/// See `docs/chain-model.md` §1 for the full glossary of chain-height
/// terms.
pub type BlockHeight = u64;

/// A block, encoded as it is on the block chain.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// The header hash of this block.
    pub block_header: BlockHeader,
    /// The transactions in this block.
    pub transactions: Vec<Arc<SignedTransaction>>,
    /// The shielded transaction bodies, a section apart from the account
    /// transactions. Their proofs travel as a witness sidecar.
    pub shielded: Vec<Arc<ShieldedTxBody>>,
    /// The witness sidecar: one proof per shielded body, in order. It is part
    /// of the network encoding only. The block database never stores it,
    /// and the block hash commits to it through each body's `proof_hash`.
    pub witness: Vec<Arc<Vec<u8>>>,
    /// Approximated rlp size of the block.
    pub approximated_rlp_size: usize,
    /// Approximated rlp size of block with transaction public key.
    pub approximated_rlp_size_with_public: usize,
}

impl MallocSizeOf for Block {
    fn size_of(&self, ops: &mut MallocSizeOfOps) -> usize {
        self.block_header.size_of(ops)
            + self.transactions.size_of(ops)
            + self.shielded.len() * std::mem::size_of::<ShieldedTxBody>()
    }
}

impl Block {
    pub fn new(
        block_header: BlockHeader, transactions: Vec<Arc<SignedTransaction>>,
    ) -> Self {
        Self::new_with_rlp_size(block_header, transactions, None, None)
    }

    pub fn new_with_rlp_size(
        block_header: BlockHeader, transactions: Vec<Arc<SignedTransaction>>,
        rlp_size: Option<usize>, rlp_size_with_public: Option<usize>,
    ) -> Self {
        let approximated_rlp_size = match rlp_size {
            Some(size) => size,
            None => transactions
                .iter()
                .fold(block_header.approximated_rlp_size(), |accum, tx| {
                    accum + tx.rlp_size()
                }),
        };

        let approximated_rlp_size_with_public = match rlp_size_with_public {
            Some(size) => size,
            None => approximated_rlp_size + transactions.len() * 84, /* Sender(20B) + Public(64B) */
        };

        Block {
            block_header,
            transactions,
            shielded: Vec::new(),
            witness: Vec::new(),
            approximated_rlp_size,
            approximated_rlp_size_with_public,
        }
    }

    /// Sets the shielded section. The approximated sizes grow by the encoded
    /// bodies.
    pub fn with_shielded(mut self, shielded: Vec<Arc<ShieldedTxBody>>) -> Self {
        let added: usize =
            shielded.iter().map(|body| rlp::encode(&**body).len()).sum();
        self.approximated_rlp_size += added;
        self.approximated_rlp_size_with_public += added;
        self.shielded = shielded;
        self
    }

    /// Sets the witness sidecar: one proof per shielded body. The
    /// approximated sizes grow by the proofs, so packets stay within bounds.
    pub fn with_witness(mut self, witness: Vec<Arc<Vec<u8>>>) -> Self {
        let added: usize = witness.iter().map(|proof| proof.len() + 4).sum();
        self.approximated_rlp_size += added;
        self.approximated_rlp_size_with_public += added;
        self.witness = witness;
        self
    }

    /// Whether this block can be sent to a peer: a shielded section needs
    /// its witness, and a block whose witness was pruned is not served.
    pub fn is_servable(&self) -> bool {
        self.shielded.is_empty() || self.has_complete_witness()
    }

    /// Whether the block carries one proof per shielded body.
    pub fn has_complete_witness(&self) -> bool {
        self.witness.len() == self.shielded.len()
    }

    /// `[header, transactions]`, plus the shielded section and its witness
    /// as a third and fourth item when the section is not empty. Blocks
    /// without shielded transactions keep their encoding.
    fn begin_block_list(&self, stream: &mut RlpStream) {
        stream.begin_list(2 + 2 * !self.shielded.is_empty() as usize);
    }

    fn append_shielded(&self, stream: &mut RlpStream) {
        if !self.shielded.is_empty() {
            stream.begin_list(self.shielded.len());
            for body in &self.shielded {
                stream.append(&**body);
            }
            stream.begin_list(self.witness.len());
            for proof in &self.witness {
                stream.append(&proof.as_slice());
            }
        }
    }

    fn decode_shielded(
        rlp: &Rlp,
    ) -> Result<(Vec<Arc<ShieldedTxBody>>, Vec<Arc<Vec<u8>>>), DecoderError>
    {
        match rlp.item_count()? {
            2 => Ok((Vec::new(), Vec::new())),
            4 => {
                let bodies = rlp.list_at::<ShieldedTxBody>(2)?;
                // An empty section has only one form: absent.
                if bodies.is_empty() {
                    return Err(DecoderError::Custom(
                        "Empty shielded section must be omitted",
                    ));
                }
                let proofs = rlp.at(3)?;
                if proofs.item_count()? != bodies.len() {
                    return Err(DecoderError::Custom(
                        "One proof per shielded body",
                    ));
                }
                let mut witness = Vec::with_capacity(bodies.len());
                for proof in proofs.iter() {
                    let bytes = proof.data()?;
                    if bytes.len() > MAX_PROOF_BYTES {
                        return Err(DecoderError::Custom("Proof too long"));
                    }
                    witness.push(Arc::new(bytes.to_vec()));
                }
                Ok((bodies.into_iter().map(Arc::new).collect(), witness))
            }
            _ => Err(DecoderError::RlpIncorrectListLen),
        }
    }

    pub fn hash(&self) -> H256 {
        self.block_header.hash()
    }

    /// Approximated rlp size of the block.
    pub fn approximated_rlp_size(&self) -> usize {
        self.approximated_rlp_size
    }

    /// Approximated rlp size of block with transaction public key.
    pub fn approximated_rlp_size_with_public(&self) -> usize {
        self.approximated_rlp_size_with_public
    }

    pub fn total_gas(&self) -> U256 {
        let mut sum = U256::from(0);
        for t in &self.transactions {
            sum += *t.gas();
        }
        sum
    }

    /// The size filled in the RPC response. It returns the approximate rlp size
    /// of the block.
    pub fn size(&self) -> usize {
        // FIXME: Because the approximate rlp size of the header may deviate
        // FIXME: from the real rlp, now we always recalculate this to
        // FIXME: avoid it failing the test case. One possible long term
        // FIXME: correct solution is to implement a size calculation
        // FIXME: that is consistent with the rlp.
        self.transactions
            .iter()
            .fold(0, |accum, tx| accum + tx.rlp_size())
    }

    pub fn transaction_hashes(&self, space_filter: Option<Space>) -> Vec<H256> {
        if let Some(space) = space_filter {
            self.transactions
                .iter()
                .filter(|tx| tx.space() == space)
                .map(|tx| tx.hash())
                .collect()
        } else {
            self.transactions.iter().map(|tx| tx.hash()).collect()
        }
    }

    /// Construct a new compact block with random nonce
    /// This block will be relayed with the new compact block to prevent
    /// adversaries to make tx shortId collision
    pub fn to_compact(&self) -> CompactBlock {
        let nonce: u64 = rand::thread_rng().gen();
        let (k0, k1) =
            CompactBlock::get_shortid_key(&self.block_header, &nonce);
        CompactBlock {
            block_header: self.block_header.clone(),
            nonce,
            tx_short_ids: CompactBlock::create_shortids(
                &self.transactions,
                k0,
                k1,
            ),
            // reconstructed_txns constructed here will not be used
            reconstructed_txns: Vec::new(),
            shielded: self.shielded.clone(),
            witness: self.witness.clone(),
        }
    }

    pub fn encode_body_with_tx_public(&self) -> Vec<u8> {
        let mut stream = RlpStream::new();
        stream.begin_list(self.transactions.len());
        for tx in &self.transactions {
            stream.append(tx.as_ref());
        }
        stream.drain()
    }

    pub fn decode_body_with_tx_public(
        rlp: &Rlp,
    ) -> Result<Vec<Arc<SignedTransaction>>, DecoderError> {
        if rlp.as_raw().len() != rlp.payload_info()?.total() {
            return Err(DecoderError::RlpIsTooBig);
        }

        let signed_transactions = rlp.as_list()?;
        let mut transactions = Vec::with_capacity(signed_transactions.len());
        for tx in signed_transactions {
            transactions.push(Arc::new(tx));
        }

        Ok(transactions)
    }

    /// The body as the block database stores it. Without shielded
    /// transactions it is the transaction list, unchanged. With them it is
    /// `STORED_BODY_WITH_SHIELDED ‖ rlp([transactions, shielded])`; the
    /// marker is a single-byte RLP item, which can never start the list of
    /// the old format, so the two are unambiguous.
    pub fn encode_stored_body(&self) -> Vec<u8> {
        if self.shielded.is_empty() {
            return self.encode_body_with_tx_public();
        }
        let mut stream = RlpStream::new_list(2);
        stream.append_raw(&self.encode_body_with_tx_public(), 1);
        stream.begin_list(self.shielded.len());
        for body in &self.shielded {
            stream.append(&**body);
        }
        let mut out = vec![STORED_BODY_WITH_SHIELDED];
        out.extend_from_slice(&stream.out());
        out
    }

    pub fn decode_stored_body(
        bytes: &[u8],
    ) -> Result<
        (Vec<Arc<SignedTransaction>>, Vec<Arc<ShieldedTxBody>>),
        DecoderError,
    > {
        match bytes.first() {
            Some(&STORED_BODY_WITH_SHIELDED) => {
                let rlp = Rlp::new(&bytes[1..]);
                if rlp.as_raw().len() != rlp.payload_info()?.total() {
                    return Err(DecoderError::RlpIsTooBig);
                }
                if rlp.item_count()? != 2 {
                    return Err(DecoderError::RlpIncorrectListLen);
                }
                let transactions =
                    Self::decode_body_with_tx_public(&rlp.at(0)?)?;
                let shielded = rlp.list_at::<ShieldedTxBody>(1)?;
                if shielded.is_empty() {
                    return Err(DecoderError::Custom(
                        "Empty shielded section must be omitted",
                    ));
                }
                Ok((transactions, shielded.into_iter().map(Arc::new).collect()))
            }
            _ => Ok((
                Self::decode_body_with_tx_public(&Rlp::new(bytes))?,
                Vec::new(),
            )),
        }
    }

    pub fn encode_with_tx_public(&self) -> Vec<u8> {
        let mut stream = RlpStream::new();
        self.begin_block_list(&mut stream);
        stream
            .append(&self.block_header)
            .append_raw(&*self.encode_body_with_tx_public(), 1);
        self.append_shielded(&mut stream);
        stream.drain()
    }

    pub fn decode_with_tx_public(rlp: &Rlp) -> Result<Self, DecoderError> {
        if rlp.as_raw().len() != rlp.payload_info()?.total() {
            return Err(DecoderError::RlpIsTooBig);
        }
        let (shielded, witness) = Self::decode_shielded(rlp)?;

        let mut block = Block::new_with_rlp_size(
            rlp.val_at(0)?,
            Self::decode_body_with_tx_public(&rlp.at(1)?)?,
            None,
            Some(rlp.as_raw().len()),
        );
        block.shielded = shielded;
        block.witness = witness;
        Ok(block)
    }
}

// The encode of Block only serializes TransactionWithSignature
// without "sender" and "public" fields.
impl Encodable for Block {
    fn rlp_append(&self, stream: &mut RlpStream) {
        self.begin_block_list(stream);
        stream.append(&self.block_header);
        stream.begin_list(self.transactions.len());
        for tx in &self.transactions {
            stream.append(&tx.transaction);
        }
        self.append_shielded(stream);
    }
}

// The decode of Block only deserializes TransactionWithSignature
// without "sender" and "public" fields. So need to recover public later.
impl Decodable for Block {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        if rlp.as_raw().len() != rlp.payload_info()?.total() {
            return Err(DecoderError::RlpIsTooBig);
        }
        let (shielded, witness) = Self::decode_shielded(rlp)?;

        let transactions = rlp.list_at::<TransactionWithSignature>(1)?;

        let mut signed_transactions = Vec::with_capacity(transactions.len());
        for tx in transactions {
            let signed = SignedTransaction::new_unsigned(tx);
            signed_transactions.push(Arc::new(signed));
        }

        let mut block = Block::new_with_rlp_size(
            rlp.val_at(0)?,
            signed_transactions,
            Some(rlp.as_raw().len()),
            None,
        );
        block.shielded = shielded;
        block.witness = witness;
        Ok(block)
    }
}

// TODO Some optimization may be made if short_id hash collission is detected,
// but should be rare
#[derive(Clone, PartialEq)]
pub struct CompactBlock {
    /// The block header
    pub block_header: BlockHeader,
    /// The nonce for use in short id calculation
    pub nonce: u64,
    /// A list of tx short ids
    pub tx_short_ids: Vec<u8>,
    /// Store the txns reconstructed, None means not received
    pub reconstructed_txns: Vec<Option<Arc<SignedTransaction>>>,
    /// The shielded section, carried whole: bodies are small and there is
    /// no mempool from which to rebuild them.
    pub shielded: Vec<Arc<ShieldedTxBody>>,
    /// The section's witness, so the rebuilt block can be verified.
    pub witness: Vec<Arc<Vec<u8>>>,
}

impl Debug for CompactBlock {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(
            f,
            "CompactBlock{{ block_header: {:?}, nonce: {:?}}}",
            self.block_header, self.nonce
        )
    }
}

impl MallocSizeOf for CompactBlock {
    fn size_of(&self, ops: &mut MallocSizeOfOps) -> usize {
        self.tx_short_ids.size_of(ops) + self.reconstructed_txns.size_of(ops)
    }
}

impl CompactBlock {
    const SHORT_ID_SIZE_IN_BYTES: usize = 6;

    pub fn len(&self) -> usize {
        self.tx_short_ids.len() / CompactBlock::SHORT_ID_SIZE_IN_BYTES
    }

    pub fn hash(&self) -> H256 {
        self.block_header.hash()
    }

    pub fn get_shortid_key(header: &BlockHeader, nonce: &u64) -> (u64, u64) {
        let mut stream = RlpStream::new();
        stream.begin_list(2).append(header).append(nonce);
        let to_hash = stream.out();
        let key_hash: [u8; 32] = keccak(to_hash).into();
        let k0 = LittleEndian::read_u64(&key_hash[0..8]);
        let k1 = LittleEndian::read_u64(&key_hash[8..16]);
        (k0, k1)
    }

    /// Compute Tx ShortId from hash
    pub fn create_shortids(
        transactions: &Vec<Arc<SignedTransaction>>, k0: u64, k1: u64,
    ) -> Vec<u8> {
        let mut short_ids: Vec<u8> = vec![];

        for tx in transactions {
            let hash = tx.hash();
            let random = CompactBlock::get_random_bytes(&hash, k0, k1);

            short_ids.push(((random & 0xff00) >> 8) as u8);
            short_ids.push((random & 0xff) as u8);
            short_ids.push(hash[28]);
            short_ids.push(hash[29]);
            short_ids.push(hash[30]);
            short_ids.push(hash[31]);
        }
        short_ids
    }

    pub fn to_u16(v1: u8, v2: u8) -> u16 {
        ((v1 as u16) << 8) + v2 as u16
    }

    pub fn to_u32(v1: u8, v2: u8, v3: u8, v4: u8) -> u32 {
        ((v1 as u32) << 24)
            + ((v2 as u32) << 16)
            + ((v3 as u32) << 8)
            + v4 as u32
    }

    pub fn get_random_bytes(
        transaction_id: &H256, key1: u64, key2: u64,
    ) -> u16 {
        let mut hasher = SipHasher24::new_with_keys(key1, key2);
        hasher.write(transaction_id.as_ref());
        (hasher.finish() & 0xffff) as u16
    }

    pub fn get_decomposed_short_ids(&self) -> (Vec<u16>, Vec<u32>) {
        let mut random_bytes_vector: Vec<u16> = Vec::new();
        let mut fixed_bytes_vector: Vec<u32> = Vec::new();

        for i in (0..self.tx_short_ids.len())
            .step_by(CompactBlock::SHORT_ID_SIZE_IN_BYTES)
        {
            random_bytes_vector.push(CompactBlock::to_u16(
                self.tx_short_ids[i],
                self.tx_short_ids[i + 1],
            ));
            fixed_bytes_vector.push(CompactBlock::to_u32(
                self.tx_short_ids[i + 2],
                self.tx_short_ids[i + 3],
                self.tx_short_ids[i + 4],
                self.tx_short_ids[i + 5],
            ));
        }

        (random_bytes_vector, fixed_bytes_vector)
    }
}

impl Encodable for CompactBlock {
    fn rlp_append(&self, steam: &mut RlpStream) {
        steam
            .begin_list(3 + 2 * !self.shielded.is_empty() as usize)
            .append(&self.block_header)
            .append(&self.nonce)
            .append(&self.tx_short_ids);
        if !self.shielded.is_empty() {
            steam.begin_list(self.shielded.len());
            for body in &self.shielded {
                steam.append(&**body);
            }
            steam.begin_list(self.witness.len());
            for proof in &self.witness {
                steam.append(&proof.as_slice());
            }
        }
    }
}

impl Decodable for CompactBlock {
    fn decode(rlp: &Rlp) -> Result<Self, DecoderError> {
        let short_ids: Vec<u8> = rlp.val_at(2)?;
        if short_ids.len() % CompactBlock::SHORT_ID_SIZE_IN_BYTES != 0 {
            return Err(DecoderError::Custom("Compact Block length Error!"));
        }
        let (shielded, witness) = match rlp.item_count()? {
            3 => (Vec::new(), Vec::new()),
            5 => {
                let bodies = rlp.list_at::<ShieldedTxBody>(3)?;
                if bodies.is_empty() {
                    return Err(DecoderError::Custom(
                        "Empty shielded section must be omitted",
                    ));
                }
                let proofs = rlp.at(4)?;
                if proofs.item_count()? != bodies.len() {
                    return Err(DecoderError::Custom(
                        "One proof per shielded body",
                    ));
                }
                let mut witness = Vec::with_capacity(bodies.len());
                for proof in proofs.iter() {
                    let bytes = proof.data()?;
                    if bytes.len() > MAX_PROOF_BYTES {
                        return Err(DecoderError::Custom("Proof too long"));
                    }
                    witness.push(Arc::new(bytes.to_vec()));
                }
                (bodies.into_iter().map(Arc::new).collect(), witness)
            }
            _ => return Err(DecoderError::RlpIncorrectListLen),
        };
        Ok(CompactBlock {
            block_header: rlp.val_at(0)?,
            nonce: rlp.val_at(1)?,
            tx_short_ids: short_ids,
            reconstructed_txns: Vec::new(),
            shielded,
            witness,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Block;
    use crate::{shielded::ShieldedTxBody, BlockHeaderBuilder};
    use rlp::{Rlp, RlpStream};
    use std::sync::Arc;

    fn block(shielded: usize) -> Block {
        let header = BlockHeaderBuilder::new().with_height(7).build();
        Block::new(header, vec![])
            .with_shielded(
                (0..shielded as u32)
                    .map(|i| Arc::new(ShieldedTxBody::sample_for_tests(i)))
                    .collect(),
            )
            .with_witness(
                (0..shielded).map(|i| Arc::new(vec![i as u8; 100])).collect(),
            )
    }

    #[test]
    fn block_without_shielded_keeps_two_items() {
        let b = block(0);
        for encoded in [rlp::encode(&b).to_vec(), b.encode_with_tx_public()] {
            assert_eq!(Rlp::new(&encoded).item_count().unwrap(), 2);
        }
    }

    #[test]
    fn shielded_section_round_trips() {
        let b = block(3);
        let encoded = rlp::encode(&b);
        assert_eq!(Rlp::new(&encoded).item_count().unwrap(), 4);
        let decoded: Block = rlp::decode(&encoded).unwrap();
        assert_eq!(decoded.shielded, b.shielded);
        assert_eq!(decoded.witness, b.witness);
        assert_eq!(decoded.hash(), b.hash());

        let with_public = b.encode_with_tx_public();
        let decoded =
            Block::decode_with_tx_public(&Rlp::new(&with_public)).unwrap();
        assert_eq!(decoded.shielded, b.shielded);
        assert_eq!(decoded.witness, b.witness);

        // The stored body never carries the witness.
        let (_, shielded) = Block::decode_stored_body(&b.encode_stored_body()).unwrap();
        assert_eq!(shielded, b.shielded);
    }

    #[test]
    fn the_witness_must_match_the_section() {
        // Three items (section without witness) and a witness of the wrong
        // length are both refused, as is an oversized proof.
        let b = block(2);
        let mut s = RlpStream::new_list(3);
        s.append(&b.block_header);
        s.begin_list(0);
        s.append_list(&b.shielded.iter().map(|x| (**x).clone()).collect::<Vec<_>>());
        assert!(rlp::decode::<Block>(&s.out()).is_err());

        let short = block(2).with_witness(vec![Arc::new(vec![1u8; 10])]);
        assert!(rlp::decode::<Block>(&rlp::encode(&short)).is_err());

        let huge = block(1).with_witness(vec![Arc::new(vec![
            0u8;
            crate::shielded::MAX_PROOF_BYTES + 1
        ])]);
        assert!(rlp::decode::<Block>(&rlp::encode(&huge)).is_err());
    }

    #[test]
    fn empty_shielded_section_is_rejected() {
        let b = block(0);
        let mut s = RlpStream::new_list(4);
        s.append(&b.block_header);
        s.begin_list(0);
        s.begin_list(0);
        s.begin_list(0);
        let encoded = s.out();
        assert!(rlp::decode::<Block>(&encoded).is_err());
        assert!(Block::decode_with_tx_public(&Rlp::new(&encoded)).is_err());
    }

    #[test]
    fn stored_body_keeps_the_old_format_and_the_shielded_section() {
        // Without shielded transactions: the old body, byte for byte.
        let plain = block(0);
        assert_eq!(plain.encode_stored_body(), plain.encode_body_with_tx_public());
        let (txs, shielded) =
            Block::decode_stored_body(&plain.encode_stored_body()).unwrap();
        assert!(txs.is_empty() && shielded.is_empty());

        let b = block(2);
        let stored = b.encode_stored_body();
        assert_eq!(stored[0], super::STORED_BODY_WITH_SHIELDED);
        let (_, shielded) = Block::decode_stored_body(&stored).unwrap();
        assert_eq!(shielded, b.shielded);

        let mut longer = stored.clone();
        longer.push(0);
        assert!(Block::decode_stored_body(&longer).is_err());
        let mut s = RlpStream::new_list(2);
        s.begin_list(0);
        s.begin_list(0);
        let mut empty = vec![super::STORED_BODY_WITH_SHIELDED];
        empty.extend_from_slice(&s.out());
        assert!(Block::decode_stored_body(&empty).is_err());
    }

    #[test]
    fn compact_block_carries_the_shielded_section() {
        let plain = block(0).to_compact();
        let encoded = rlp::encode(&plain);
        assert_eq!(Rlp::new(&encoded).item_count().unwrap(), 3);

        let b = block(2);
        let compact = b.to_compact();
        let decoded: super::CompactBlock =
            rlp::decode(&rlp::encode(&compact)).unwrap();
        assert_eq!(decoded.shielded, b.shielded);
        assert_eq!(decoded.witness, b.witness);
        assert_eq!(decoded.hash(), b.hash());
    }

    #[test]
    fn five_items_are_rejected() {
        let b = block(1);
        let mut s = RlpStream::new_list(5);
        s.append(&b.block_header);
        s.begin_list(0);
        s.append_list(&[ShieldedTxBody::sample_for_tests(1)]);
        s.begin_list(1).append(&vec![1u8; 4]);
        s.begin_list(0);
        assert!(rlp::decode::<Block>(&s.out()).is_err());
    }
}
