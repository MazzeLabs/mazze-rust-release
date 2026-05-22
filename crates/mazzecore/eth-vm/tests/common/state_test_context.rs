// Copyright 2025 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! `StateTestContext` — a `vm::Context` implementation that holds per-address
//! state in memory. Used by the Ethereum state-test harness to feed `RevmExec`
//! a known pre-state and inspect the post-state after `exec()` returns.
//!
//! This is intentionally separate from the production `executor::Context`
//! (which is wired to Mazze's `State`, `Substate`, `Spec`, etc.); state tests
//! exercise only the EVM correctness contract — Mazze-specific economics
//! (sponsor, collateral, cross-space) are out of scope here.

use keccak_hash::keccak;
use mazze_bytes::Bytes;
use mazze_db_errors::statedb::Result as DbResult;
use mazze_types::{Address, Space, H256, U256};
use mazze_vm_types::{
    BlockHashSource, CallType, Context as VmContext, ContractCreateResult,
    CreateContractAddress, Env, Error as VmError, GasLeft, InterpreterInfo,
    MessageCallResult, Result as VmResult, ReturnData, Spec, TrapKind,
};
use std::collections::HashMap;
use std::sync::Arc;

/// Snapshot of one account's state. Mirrors what the Ethereum state-test
/// JSON format calls a `pre`/`post` account.
#[derive(Default, Clone, Debug)]
pub struct StateAccount {
    pub nonce: U256,
    pub balance: U256,
    pub code: Vec<u8>,
    pub storage: HashMap<H256, U256>,
}

#[derive(Clone, Debug)]
pub struct EmittedLog {
    pub address: Address,
    pub topics: Vec<H256>,
    pub data: Vec<u8>,
}

/// A self-contained `vm::Context` whose state lives in `HashMap`s.
///
/// Implements every by-address state method `RevmExec` calls. The
/// origin-pinned legacy methods (`storage_at`, `set_storage`, `log`,
/// `create`, `call`, `ret`, `suicide`) are deliberately stubbed —
/// `RevmExec` does not invoke them.
pub struct StateTestContext {
    pub accounts: HashMap<Address, StateAccount>,
    pub logs: Vec<EmittedLog>,
    pub env: Env,
    pub spec: Spec,
    pub chain_id: u64,
}

impl StateTestContext {
    pub fn new(env: Env, spec: Spec, chain_id: u64) -> Self {
        Self {
            accounts: HashMap::new(),
            logs: Vec::new(),
            env,
            spec,
            chain_id,
        }
    }

    /// Convenience for `pre`-state population: insert a full account.
    pub fn insert_account(&mut self, addr: Address, account: StateAccount) {
        self.accounts.insert(addr, account);
    }

    /// Increment the sender's nonce to mirror Mazze's production
    /// pre-VM `inc_sender_nonce`. RevmExec's database adapter
    /// compensates by reporting `nonce-1` to revm — see
    /// [`MazzeDatabase::basic`]. U256 has plenty of headroom so this
    /// never overflows; the actual nonce-overflow check fires when
    /// revm tries to widen the post-increment value to u64.
    pub fn bump_sender_nonce(&mut self, addr: Address) {
        let acct = self.account_mut_or_insert(&addr);
        acct.nonce = acct.nonce + U256::one();
    }

    fn account(&self, addr: &Address) -> Option<&StateAccount> {
        self.accounts.get(addr)
    }

    fn account_mut_or_insert(&mut self, addr: &Address) -> &mut StateAccount {
        self.accounts.entry(*addr).or_default()
    }
}

impl VmContext for StateTestContext {
    // ---- Legacy origin-pinned methods. Not used by RevmExec. ----

    fn storage_at(&self, _key: &Vec<u8>) -> VmResult<U256> {
        Err(VmError::Wasm(
            "StateTestContext: storage_at not supported (use storage_at_address)".into(),
        ))
    }

    fn set_storage(&mut self, _key: Vec<u8>, _value: U256) -> VmResult<()> {
        Err(VmError::Wasm(
            "StateTestContext: set_storage not supported (use set_storage_at_address)".into(),
        ))
    }

    fn transient_storage_at(&self, _key: &Vec<u8>) -> VmResult<U256> {
        Ok(U256::zero())
    }

    fn transient_set_storage(
        &mut self, _key: Vec<u8>, _value: U256,
    ) -> VmResult<()> {
        Ok(())
    }

    fn log(&mut self, _topics: Vec<H256>, _data: &[u8]) -> VmResult<()> {
        Err(VmError::Wasm(
            "StateTestContext: log not supported (use log_for_address)".into(),
        ))
    }

    fn create(
        &mut self, _gas: &U256, _value: &U256, _code: &[u8],
        _address: CreateContractAddress,
    ) -> DbResult<std::result::Result<ContractCreateResult, TrapKind>> {
        unimplemented!("RevmExec handles CREATE inside revm; not called via Context")
    }

    fn call(
        &mut self, _gas: &U256, _sender_address: &Address,
        _receive_address: &Address, _value: Option<U256>, _data: &[u8],
        _code_address: &Address, _call_type: CallType,
    ) -> DbResult<std::result::Result<MessageCallResult, TrapKind>> {
        unimplemented!("RevmExec handles CALL inside revm; not called via Context")
    }

    fn ret(
        self, _gas: &U256, _data: &ReturnData, _apply_state: bool,
    ) -> VmResult<U256> {
        unimplemented!("RevmExec returns GasLeft directly; ret() not called")
    }

    fn suicide(&mut self, _refund_address: &Address) -> VmResult<()> {
        // SELFDESTRUCT in revm is applied through the EvmState diff
        // (account.is_selfdestructed()). RevmExec handles it there; we
        // never reach this hook.
        Ok(())
    }

    fn origin_balance(&self) -> VmResult<U256> {
        Ok(U256::zero())
    }

    fn depth(&self) -> usize {
        0
    }

    fn is_static(&self) -> bool {
        false
    }

    fn is_static_or_reentrancy(&self) -> bool {
        false
    }

    fn blockhash_source(&self) -> BlockHashSource {
        BlockHashSource::Env
    }

    // ---- Address-, env-, spec- methods that RevmExec actually uses. ----

    fn exists(&self, address: &Address) -> VmResult<bool> {
        Ok(self.account(address).is_some())
    }

    fn exists_and_not_null(&self, address: &Address) -> VmResult<bool> {
        Ok(self
            .account(address)
            .map(|a| !a.balance.is_zero() || !a.nonce.is_zero() || !a.code.is_empty())
            .unwrap_or(false))
    }

    fn balance(&self, address: &Address) -> VmResult<U256> {
        Ok(self.account(address).map(|a| a.balance).unwrap_or_default())
    }

    fn blockhash(&mut self, _number: &U256) -> VmResult<H256> {
        Ok(self.env.last_hash)
    }

    fn extcode(&self, address: &Address) -> VmResult<Option<Arc<Bytes>>> {
        Ok(self
            .account(address)
            .filter(|a| !a.code.is_empty())
            .map(|a| Arc::new(a.code.clone())))
    }

    fn extcodehash(&self, address: &Address) -> VmResult<H256> {
        Ok(self
            .account(address)
            .map(|a| {
                if a.code.is_empty() {
                    H256::zero()
                } else {
                    keccak(&a.code)
                }
            })
            .unwrap_or_else(H256::zero))
    }

    fn extcodesize(&self, address: &Address) -> VmResult<usize> {
        Ok(self.account(address).map(|a| a.code.len()).unwrap_or(0))
    }

    fn spec(&self) -> &Spec {
        &self.spec
    }

    fn env(&self) -> &Env {
        &self.env
    }

    fn chain_id(&self) -> u64 {
        self.chain_id
    }

    fn space(&self) -> Space {
        Space::Ethereum
    }

    fn trace_step(&mut self, _interpreter: &dyn InterpreterInfo) {}
    fn trace_step_end(&mut self, _interpreter: &dyn InterpreterInfo) {}
    fn opcode_trace_enabled(&self) -> bool {
        false
    }

    // ---- The by-address methods RevmExec uses to read + apply state. ----

    fn storage_at_address(
        &self, address: &Address, key: &[u8],
    ) -> VmResult<U256> {
        let slot = if key.len() == 32 {
            H256::from_slice(key)
        } else {
            return Ok(U256::zero());
        };
        Ok(self
            .account(address)
            .and_then(|a| a.storage.get(&slot).copied())
            .unwrap_or_default())
    }

    fn set_storage_at_address(
        &mut self, address: &Address, key: Vec<u8>, value: U256,
    ) -> VmResult<()> {
        if key.len() != 32 {
            return Err(VmError::Wasm(format!(
                "StateTestContext: bad key length {}",
                key.len()
            )));
        }
        let slot = H256::from_slice(&key);
        let acct = self.account_mut_or_insert(address);
        if value.is_zero() {
            acct.storage.remove(&slot);
        } else {
            acct.storage.insert(slot, value);
        }
        Ok(())
    }

    fn nonce_of(&self, address: &Address) -> VmResult<U256> {
        Ok(self.account(address).map(|a| a.nonce).unwrap_or_default())
    }

    fn set_balance(
        &mut self, address: &Address, value: U256,
    ) -> VmResult<()> {
        self.account_mut_or_insert(address).balance = value;
        Ok(())
    }

    fn set_nonce(&mut self, address: &Address, value: U256) -> VmResult<()> {
        self.account_mut_or_insert(address).nonce = value;
        Ok(())
    }

    fn set_code(&mut self, address: &Address, code: Vec<u8>) -> VmResult<()> {
        self.account_mut_or_insert(address).code = code;
        Ok(())
    }

    fn log_for_address(
        &mut self, address: &Address, topics: Vec<H256>, data: &[u8],
    ) -> VmResult<()> {
        self.logs.push(EmittedLog {
            address: *address,
            topics,
            data: data.to_vec(),
        });
        Ok(())
    }

    fn suicide_for_address(
        &mut self, contract: &Address, _refund: &Address,
    ) -> VmResult<()> {
        // For the state-test harness we just remove the account; the
        // upstream Ethereum tests already encode the post-state with the
        // dead account gone, so this matches their expected behavior.
        self.accounts.remove(contract);
        Ok(())
    }
}

/// Account-equality helper used by the state-test runner to compare
/// post-state. Compares everything except `code_hash` (we compare `code`
/// bytes directly).
pub fn accounts_equal(a: &StateAccount, b: &StateAccount) -> bool {
    a.nonce == b.nonce
        && a.balance == b.balance
        && a.code == b.code
        && a.storage == b.storage
}

/// Pretty-print account differences for failure messages.
pub fn account_diff(a: &StateAccount, b: &StateAccount) -> String {
    let mut diff = Vec::new();
    if a.nonce != b.nonce {
        diff.push(format!("nonce: {} vs {}", a.nonce, b.nonce));
    }
    if a.balance != b.balance {
        diff.push(format!("balance: {} vs {}", a.balance, b.balance));
    }
    if a.code != b.code {
        diff.push(format!(
            "code: {}B vs {}B",
            a.code.len(),
            b.code.len()
        ));
    }
    if a.storage != b.storage {
        diff.push(format!(
            "storage: {} slots vs {} slots",
            a.storage.len(),
            b.storage.len()
        ));
    }
    diff.join(", ")
}
