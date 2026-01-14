// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::{internal_bail, state::State, substate::cleanup_mode};
use crate::shielded::{fr_from_h256, fr_from_u256, fr_to_h256, poseidon_hash2};
use ark_bls12_381::{Bls12_381, Fr};
use ark_ff::PrimeField;
use ark_groth16::{
    prepare_verifying_key, Groth16, PreparedVerifyingKey, Proof,
    VerifyingKey,
};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use mazze_parameters::internal_contract_addresses::SHIELDED_POOL_CONTRACT_ADDRESS;
use mazze_types::{Address, AddressSpaceUtil, BigEndianHash, H256, U256};
use once_cell::sync::OnceCell;
use parking_lot::RwLock;
use solidity_abi::{read_abi_list, ABIDecodable, ABIDecodeError};
use std::sync::Arc;

use super::preludes::*;

type Bytes = Vec<u8>;

const ROOT_HISTORY_LEN: u64 = 64;
const MAX_NULLIFIERS: usize = 8;
const MAX_COMMITMENTS: usize = 8;
const MAX_TRANSPARENT_OUTPUTS: usize = 8;
const SHIELDED_TREE_DEPTH: usize = 32;
const MAX_CIPHERTEXT_BYTES: usize = 512;
const MAX_PROOF_BYTES: usize = 256;
const MAX_VK_BYTES: usize = 8192;
const PUBLIC_INPUT_LEN: usize = 1
    + 3
    + MAX_NULLIFIERS
    + MAX_COMMITMENTS
    + MAX_TRANSPARENT_OUTPUTS
    + MAX_TRANSPARENT_OUTPUTS
    + 1;

struct VkCache {
    hash: H256,
    pvk: Arc<PreparedVerifyingKey<Bls12_381>>,
}

static VK_CACHE: OnceCell<RwLock<Option<VkCache>>> = OnceCell::new();

#[derive(Clone, Debug)]
pub struct ShieldedBundleInput {
    anchor: H256,
    nullifiers: Vec<H256>,
    commitments: Vec<H256>,
    ciphertexts: Vec<Bytes>,
    outputs: Vec<Address>,
    values: Vec<U256>,
    fee: U256,
    proof: Bytes,
}

impl ABIDecodable for ShieldedBundleInput {
    fn abi_decode(data: &[u8]) -> Result<Self, ABIDecodeError> {
        let mut pointer = data.iter();
        Ok(ShieldedBundleInput {
            anchor: read_abi_list::<H256>(data, &mut pointer)?,
            nullifiers: read_abi_list::<Vec<H256>>(data, &mut pointer)?,
            commitments: read_abi_list::<Vec<H256>>(data, &mut pointer)?,
            ciphertexts: read_abi_list::<Vec<Bytes>>(data, &mut pointer)?,
            outputs: read_abi_list::<Vec<Address>>(data, &mut pointer)?,
            values: read_abi_list::<Vec<U256>>(data, &mut pointer)?,
            fee: read_abi_list::<U256>(data, &mut pointer)?,
            proof: read_abi_list::<Bytes>(data, &mut pointer)?,
        })
    }
}

make_solidity_contract! {
    pub struct ShieldedPool(SHIELDED_POOL_CONTRACT_ADDRESS, generate_fn_table, initialize: |_params: &CommonParams| 0, is_active: |_spec: &Spec| true);
}

fn generate_fn_table() -> SolFnTable {
    make_function_table!(
        Shield,
        ApplyShieldedBundle,
        Root,
        IsNullifierSpent,
        SetVerifyingKey,
        VerifyingKeyHash
    )
}

group_impl_is_active!(
    |_spec: &Spec| true,
    Shield,
    ApplyShieldedBundle,
    Root,
    IsNullifierSpent,
    SetVerifyingKey,
    VerifyingKeyHash
);

make_solidity_event! {
    pub struct ShieldedNoteEvent(
        "ShieldedNote(bytes32,bytes)",
        indexed: H256,
        non_indexed: Bytes
    );
}

make_solidity_function! {
    pub struct Shield((H256, Bytes), "shield(bytes32,bytes)");
}

impl_function_type!(Shield, "payable_write");

impl UpfrontPaymentTrait for Shield {
    fn upfront_gas_payment(
        &self, (_commitment, ciphertext): &(H256, Bytes), _params: &ActionParams,
        context: &InternalRefContext,
    ) -> DbResult<U256> {
        let ciphertext_words =
            (ciphertext.len().saturating_add(31) / 32) as u64;
        Ok(U256::from(context.spec.sstore_reset_gas) * U256::from(2u64)
            + U256::from(context.spec.sha3_gas) * U256::from(ciphertext_words))
    }
}

impl SimpleExecutionTrait for Shield {
    fn execute_inner(
        &self, (commitment, ciphertext): (H256, Bytes), params: &ActionParams,
        context: &mut InternalRefContext,
    ) -> vm::Result<()> {
        let value = params.value.value();
        if value.is_zero() {
            internal_bail!("shielding requires non-zero value");
        }
        if ciphertext.is_empty() {
            internal_bail!("ciphertext required for shielded deposit");
        }
        if ciphertext.len() > MAX_CIPHERTEXT_BYTES {
            internal_bail!("ciphertext too large");
        }

        let root = append_commitment(context.state, &commitment)?;
        push_root(context.state, &root)?;

        ShieldedNoteEvent::log(&commitment, &ciphertext, params, context)?;

        Ok(())
    }
}

make_solidity_function! {
    pub struct ApplyShieldedBundle(
        ShieldedBundleInput,
        "applyShieldedBundle(bytes32,bytes32[],bytes32[],bytes[],address[],uint256[],uint256,bytes)"
    );
}

impl_function_type!(ApplyShieldedBundle, "non_payable_write");

impl UpfrontPaymentTrait for ApplyShieldedBundle {
    fn upfront_gas_payment(
        &self,
        bundle: &ShieldedBundleInput,
        _params: &ActionParams,
        context: &InternalRefContext,
    ) -> DbResult<U256> {
        let writes = (bundle.nullifiers.len()
            + bundle.commitments.len()
            + bundle.outputs.len()
            + 4) as u64;
        let proof_words = (bundle.proof.len().saturating_add(31) / 32) as u64;
        let ciphertext_words = bundle
            .ciphertexts
            .iter()
            .map(|entry| entry.len().saturating_add(31) / 32)
            .sum::<usize>() as u64;
        Ok(U256::from(context.spec.sstore_reset_gas) * U256::from(writes)
            + U256::from(context.spec.sha3_gas)
                * U256::from(proof_words + ciphertext_words))
    }
}

impl SimpleExecutionTrait for ApplyShieldedBundle {
    fn execute_inner(
        &self,
        bundle: ShieldedBundleInput,
        params: &ActionParams,
        context: &mut InternalRefContext,
    ) -> vm::Result<()> {
        if bundle.outputs.len() != bundle.values.len() {
            internal_bail!("transparent outputs length mismatch");
        }
        if bundle.ciphertexts.len() != bundle.commitments.len() {
            internal_bail!("ciphertexts length mismatch");
        }
        if bundle.nullifiers.len() > MAX_NULLIFIERS {
            internal_bail!("too many nullifiers");
        }
        if bundle.commitments.len() > MAX_COMMITMENTS {
            internal_bail!("too many commitments");
        }
        if bundle.ciphertexts.len() > MAX_COMMITMENTS {
            internal_bail!("too many ciphertexts");
        }
        if bundle.outputs.len() > MAX_TRANSPARENT_OUTPUTS {
            internal_bail!("too many transparent outputs");
        }
        for ciphertext in &bundle.ciphertexts {
            if ciphertext.is_empty() {
                internal_bail!("ciphertext required for commitment");
            }
            if ciphertext.len() > MAX_CIPHERTEXT_BYTES {
                internal_bail!("ciphertext too large");
            }
        }

        if !is_root_known(context.state, &bundle.anchor)? {
            internal_bail!("unknown anchor");
        }
        for nullifier in &bundle.nullifiers {
            if is_nullifier_spent(context.state, nullifier)? {
                internal_bail!("nullifier already spent");
            }
        }

        if !verify_groth16(
            context.state,
            &bundle.anchor,
            &bundle.nullifiers,
            &bundle.commitments,
            &bundle.outputs,
            &bundle.values,
            &bundle.fee,
            &bundle.proof,
        ) {
            internal_bail!("invalid shielded proof");
        }

        for nullifier in bundle.nullifiers {
            set_nullifier_spent(context.state, &nullifier)?;
        }

        for (commitment, ciphertext) in
            bundle.commitments.iter().zip(bundle.ciphertexts.iter())
        {
            ShieldedNoteEvent::log(commitment, ciphertext, params, context)?;
        }
        let mut root = latest_root(context.state)?;
        for commitment in bundle.commitments {
            root = append_commitment(context.state, &commitment)?;
        }
        push_root(context.state, &root)?;

        let pool_address = SHIELDED_POOL_CONTRACT_ADDRESS.with_native_space();
        let mut total_out = bundle.fee;
        for amount in &bundle.values {
            if let Some(sum) = total_out.checked_add(*amount) {
                total_out = sum;
            } else {
                internal_bail!("shielded outputs overflow");
            }
        }
        if !total_out.is_zero() {
            let pool_balance = context.state.balance(&pool_address)?;
            if pool_balance < total_out {
                internal_bail!("insufficient shielded pool balance");
            }
        }

        if !bundle.fee.is_zero() {
            context.state.transfer_balance(
                &pool_address,
                &context.env.author.with_native_space(),
                &bundle.fee,
                cleanup_mode(context.substate, context.spec),
            )?;
        }

        for (address, amount) in bundle
            .outputs
            .into_iter()
            .zip(bundle.values.into_iter())
        {
            if amount.is_zero() {
                continue;
            }
            context.state.transfer_balance(
                &pool_address,
                &address.with_native_space(),
                &amount,
                cleanup_mode(context.substate, context.spec),
            )?;
        }

        Ok(())
    }
}

make_solidity_function! {
    pub struct Root((), "root()", H256);
}

impl_function_type!(Root, "query", gas: |spec: &Spec| spec.sload_gas);

impl SimpleExecutionTrait for Root {
    fn execute_inner(
        &self, _input: (), _params: &ActionParams,
        context: &mut InternalRefContext,
    ) -> vm::Result<H256> {
        latest_root(context.state).map_err(|e| e.into())
    }
}

make_solidity_function! {
    pub struct IsNullifierSpent(H256, "isNullifierSpent(bytes32)", bool);
}

impl_function_type!(IsNullifierSpent, "query", gas: |spec: &Spec| spec.sload_gas);

impl SimpleExecutionTrait for IsNullifierSpent {
    fn execute_inner(
        &self, nullifier: H256, _params: &ActionParams,
        context: &mut InternalRefContext,
    ) -> vm::Result<bool> {
        is_nullifier_spent(context.state, &nullifier).map_err(|e| e.into())
    }
}

make_solidity_function! {
    pub struct SetVerifyingKey(Bytes, "setVerifyingKey(bytes)");
}

impl_function_type!(SetVerifyingKey, "non_payable_write");

impl UpfrontPaymentTrait for SetVerifyingKey {
    fn upfront_gas_payment(
        &self, vk: &Bytes, _params: &ActionParams, context: &InternalRefContext,
    ) -> DbResult<U256> {
        let words = (vk.len().saturating_add(31) / 32) as u64;
        let writes = words + 2;
        Ok(U256::from(context.spec.sstore_reset_gas) * U256::from(writes))
    }
}

impl SimpleExecutionTrait for SetVerifyingKey {
    fn execute_inner(
        &self, vk: Bytes, params: &ActionParams,
        context: &mut InternalRefContext,
    ) -> vm::Result<()> {
        if !is_shielded_admin(context.state, &params.sender)? {
            internal_bail!("unauthorized");
        }
        if verifying_key_len(context.state)? != 0 {
            internal_bail!("verifying key already set");
        }
        if vk.is_empty() {
            internal_bail!("verifying key is empty");
        }
        if vk.len() > MAX_VK_BYTES {
            internal_bail!("verifying key too large");
        }

        let vk = if let Ok(vk) = deserialize_verifying_key(&vk) {
            vk
        } else {
            internal_bail!("invalid verifying key encoding");
        };
        if vk.gamma_abc_g1.len() != PUBLIC_INPUT_LEN + 1 {
            internal_bail!("verifying key public input mismatch");
        }

        let mut canonical = Vec::new();
        if vk.serialize_compressed(&mut canonical).is_err() {
            internal_bail!("failed to serialize verifying key");
        }
        if canonical.len() > MAX_VK_BYTES {
            internal_bail!("verifying key too large");
        }

        set_verifying_key(context.state, &canonical)?;

        let hash = keccak(&canonical);
        let pvk = Arc::new(prepare_verifying_key(&vk));
        *vk_cache().write() = Some(VkCache { hash, pvk });

        Ok(())
    }
}

make_solidity_function! {
    pub struct VerifyingKeyHash((), "verifyingKeyHash()", H256);
}

impl_function_type!(VerifyingKeyHash, "query", gas: |spec: &Spec| spec.sload_gas);

impl SimpleExecutionTrait for VerifyingKeyHash {
    fn execute_inner(
        &self, _input: (), _params: &ActionParams,
        context: &mut InternalRefContext,
    ) -> vm::Result<H256> {
        Ok(verifying_key_hash(context.state)?)
    }
}

fn verify_groth16(
    state: &State, anchor: &H256, nullifiers: &[H256],
    commitments: &[H256], outputs: &[Address], values: &[U256], fee: &U256,
    proof: &[u8],
) -> bool {
    if proof.len() > MAX_PROOF_BYTES {
        return false;
    }

    let pvk = match load_prepared_vk(state) {
        Ok(Some(pvk)) => pvk,
        _ => return false,
    };

    let proof = match deserialize_proof(proof) {
        Ok(proof) => proof,
        Err(_) => return false,
    };

    let inputs =
        build_public_inputs(anchor, nullifiers, commitments, outputs, values, fee);
    if inputs.len() != PUBLIC_INPUT_LEN {
        return false;
    }

    Groth16::<Bls12_381>::verify_proof(pvk.as_ref(), &proof, &inputs)
        .unwrap_or(false)
}

fn deserialize_proof(
    proof: &[u8],
) -> Result<Proof<Bls12_381>, ark_serialize::SerializationError> {
    let mut reader = proof;
    Proof::<Bls12_381>::deserialize_compressed(&mut reader)
}

fn deserialize_verifying_key(
    vk: &[u8],
) -> Result<VerifyingKey<Bls12_381>, ark_serialize::SerializationError> {
    let mut reader = vk;
    VerifyingKey::<Bls12_381>::deserialize_compressed(&mut reader)
}

fn build_public_inputs(
    anchor: &H256, nullifiers: &[H256], commitments: &[H256],
    outputs: &[Address], values: &[U256], fee: &U256,
) -> Vec<Fr> {
    let mut inputs = Vec::with_capacity(PUBLIC_INPUT_LEN);
    inputs.push(fr_from_h256(anchor));
    inputs.push(Fr::from(nullifiers.len() as u64));
    inputs.push(Fr::from(commitments.len() as u64));
    inputs.push(Fr::from(outputs.len() as u64));

    for idx in 0..MAX_NULLIFIERS {
        let value = nullifiers.get(idx).copied().unwrap_or_else(H256::zero);
        inputs.push(fr_from_h256(&value));
    }
    for idx in 0..MAX_COMMITMENTS {
        let value = commitments.get(idx).copied().unwrap_or_else(H256::zero);
        inputs.push(fr_from_h256(&value));
    }
    for idx in 0..MAX_TRANSPARENT_OUTPUTS {
        let value = outputs.get(idx).copied().unwrap_or_else(Address::zero);
        inputs.push(fr_from_address(&value));
    }
    for idx in 0..MAX_TRANSPARENT_OUTPUTS {
        let value = values.get(idx).copied().unwrap_or_else(U256::zero);
        inputs.push(fr_from_u256(&value));
    }

    inputs.push(fr_from_u256(fee));
    inputs
}

fn fr_from_address(value: &Address) -> Fr {
    Fr::from_be_bytes_mod_order(value.as_ref())
}

fn vk_cache() -> &'static RwLock<Option<VkCache>> {
    VK_CACHE.get_or_init(|| RwLock::new(None))
}

fn load_prepared_vk(
    state: &State,
) -> DbResult<Option<Arc<PreparedVerifyingKey<Bls12_381>>>> {
    let hash = verifying_key_hash(state)?;
    if hash.is_zero() {
        return Ok(None);
    }

    if let Some(cache) = vk_cache().read().as_ref() {
        if cache.hash == hash {
            return Ok(Some(cache.pvk.clone()));
        }
    }

    let bytes = match verifying_key_bytes(state)? {
        Some(bytes) => bytes,
        None => return Ok(None),
    };
    let vk = match deserialize_verifying_key(&bytes) {
        Ok(vk) => vk,
        Err(_) => return Ok(None),
    };
    if vk.gamma_abc_g1.len() != PUBLIC_INPUT_LEN + 1 {
        return Ok(None);
    }

    let pvk = Arc::new(prepare_verifying_key(&vk));
    *vk_cache().write() = Some(VkCache { hash, pvk: pvk.clone() });
    Ok(Some(pvk))
}

fn verifying_key_len(state: &State) -> DbResult<u64> {
    let key = storage_key(b"shielded:vk_len", &[]);
    Ok(state.get_system_storage(&key)?.as_u64())
}

fn verifying_key_hash(state: &State) -> DbResult<H256> {
    let key = storage_key(b"shielded:vk_hash", &[]);
    let value = state.get_system_storage(&key)?;
    Ok(BigEndianHash::from_uint(&value))
}

fn verifying_key_bytes(state: &State) -> DbResult<Option<Vec<u8>>> {
    let len = verifying_key_len(state)?;
    if len == 0 {
        return Ok(None);
    }
    if len as usize > MAX_VK_BYTES {
        return Ok(None);
    }
    let len = len as usize;
    let words = len.saturating_add(31) / 32;
    let mut out = vec![0u8; len];
    for idx in 0..words {
        let mut idx_bytes = [0u8; 8];
        idx_bytes.copy_from_slice(&(idx as u64).to_be_bytes());
        let key = storage_key(b"shielded:vk_word:", &idx_bytes);
        let value = state.get_system_storage(&key)?;
        let mut word = [0u8; 32];
        value.to_big_endian(&mut word);
        let start = idx * 32;
        let end = usize::min(start + 32, len);
        out[start..end].copy_from_slice(&word[..end - start]);
    }
    Ok(Some(out))
}

fn set_verifying_key(state: &mut State, vk: &[u8]) -> DbResult<()> {
    let len = vk.len() as u64;
    let len_key = storage_key(b"shielded:vk_len", &[]);
    state.set_system_storage(len_key, U256::from(len))?;

    let words = vk.len().saturating_add(31) / 32;
    for idx in 0..words {
        let start = idx * 32;
        let end = usize::min(start + 32, vk.len());
        let mut word = [0u8; 32];
        word[..end - start].copy_from_slice(&vk[start..end]);
        let mut idx_bytes = [0u8; 8];
        idx_bytes.copy_from_slice(&(idx as u64).to_be_bytes());
        let key = storage_key(b"shielded:vk_word:", &idx_bytes);
        state.set_system_storage(key, U256::from_big_endian(&word))?;
    }

    let hash = keccak(vk);
    let hash_key = storage_key(b"shielded:vk_hash", &[]);
    state.set_system_storage(hash_key, U256::from_big_endian(hash.as_ref()))
}

fn is_shielded_admin(state: &State, caller: &Address) -> DbResult<bool> {
    Ok(state.admin(&SHIELDED_POOL_CONTRACT_ADDRESS)? == *caller)
}

fn poseidon_commitment(left: &H256, right: &H256) -> H256 {
    let left_fr = fr_from_h256(left);
    let right_fr = fr_from_h256(right);
    fr_to_h256(&poseidon_hash2(&left_fr, &right_fr))
}

fn zero_hashes() -> &'static Vec<H256> {
    static ZERO_HASHES: OnceCell<Vec<H256>> = OnceCell::new();
    ZERO_HASHES.get_or_init(|| {
        let mut zeros = Vec::with_capacity(SHIELDED_TREE_DEPTH + 1);
        zeros.push(H256::zero());
        for level in 0..SHIELDED_TREE_DEPTH {
            let next = poseidon_commitment(&zeros[level], &zeros[level]);
            zeros.push(next);
        }
        zeros
    })
}

fn append_commitment(state: &mut State, commitment: &H256) -> DbResult<H256> {
    let index = leaf_index(state)?;
    let mut idx = index;
    let mut node = *commitment;
    for level in 0..SHIELDED_TREE_DEPTH {
        if (idx & 1) == 1 {
            let left = frontier_at(state, level)?;
            node = poseidon_commitment(&left, &node);
        } else {
            set_frontier_at(state, level, &node)?;
            let zero = &zero_hashes()[level];
            node = poseidon_commitment(&node, zero);
        }
        idx >>= 1;
    }

    set_leaf_index(state, index.saturating_add(1))?;
    Ok(node)
}

fn latest_root(state: &mut State) -> DbResult<H256> {
    let idx = root_index(state)?;
    if idx == 0 {
        return Ok(H256::zero());
    }
    root_at(state, (idx - 1) % ROOT_HISTORY_LEN)
}

fn push_root(state: &mut State, root: &H256) -> DbResult<()> {
    let idx = root_index(state)?;
    set_root_at(state, idx % ROOT_HISTORY_LEN, root)?;
    set_root_index(state, idx.saturating_add(1))
}

fn is_root_known(state: &mut State, root: &H256) -> DbResult<bool> {
    let idx = root_index(state)?;
    let max = if idx > ROOT_HISTORY_LEN { ROOT_HISTORY_LEN } else { idx };
    if max == 0 {
        return Ok(root.is_zero());
    }
    for slot in 0..max {
        if root_at(state, slot)? == *root {
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_nullifier_spent(state: &mut State, nullifier: &H256) -> DbResult<bool> {
    let key = storage_key(b"shielded:nullifier:", nullifier.as_ref());
    Ok(state.get_system_storage(&key)? != U256::zero())
}

fn set_nullifier_spent(state: &mut State, nullifier: &H256) -> DbResult<()> {
    let key = storage_key(b"shielded:nullifier:", nullifier.as_ref());
    state.set_system_storage(key, U256::one())
}

fn root_index(state: &mut State) -> DbResult<u64> {
    let key = storage_key(b"shielded:root_index", &[]);
    Ok(state.get_system_storage(&key)?.as_u64())
}

fn set_root_index(state: &mut State, idx: u64) -> DbResult<()> {
    let key = storage_key(b"shielded:root_index", &[]);
    state.set_system_storage(key, U256::from(idx))
}

fn leaf_index(state: &State) -> DbResult<u64> {
    let key = storage_key(b"shielded:leaf_index", &[]);
    Ok(state.get_system_storage(&key)?.as_u64())
}

fn set_leaf_index(state: &mut State, idx: u64) -> DbResult<()> {
    let key = storage_key(b"shielded:leaf_index", &[]);
    state.set_system_storage(key, U256::from(idx))
}

fn frontier_at(state: &State, level: usize) -> DbResult<H256> {
    let mut level_bytes = [0u8; 8];
    level_bytes.copy_from_slice(&(level as u64).to_be_bytes());
    let key = storage_key(b"shielded:frontier:", &level_bytes);
    let value = state.get_system_storage(&key)?;
    Ok(BigEndianHash::from_uint(&value))
}

fn set_frontier_at(state: &mut State, level: usize, value: &H256) -> DbResult<()> {
    let mut level_bytes = [0u8; 8];
    level_bytes.copy_from_slice(&(level as u64).to_be_bytes());
    let key = storage_key(b"shielded:frontier:", &level_bytes);
    let value = U256::from_big_endian(value.as_ref());
    state.set_system_storage(key, value)
}

fn root_at(state: &mut State, slot: u64) -> DbResult<H256> {
    let mut slot_bytes = [0u8; 8];
    slot_bytes.copy_from_slice(&slot.to_be_bytes());
    let key = storage_key(b"shielded:root:", &slot_bytes);
    let value = state.get_system_storage(&key)?;
    Ok(BigEndianHash::from_uint(&value))
}

fn set_root_at(state: &mut State, slot: u64, root: &H256) -> DbResult<()> {
    let mut slot_bytes = [0u8; 8];
    slot_bytes.copy_from_slice(&slot.to_be_bytes());
    let key = storage_key(b"shielded:root:", &slot_bytes);
    let value = U256::from_big_endian(root.as_ref());
    state.set_system_storage(key, value)
}

fn storage_key(prefix: &[u8], suffix: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(prefix.len() + suffix.len());
    buf.extend_from_slice(prefix);
    buf.extend_from_slice(suffix);
    keccak(buf).as_ref().to_vec()
}
