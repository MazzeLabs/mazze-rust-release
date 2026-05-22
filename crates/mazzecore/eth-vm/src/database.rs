// Copyright 2025 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! revm `Database` adapter over Mazze's `vm::Context`.
//!
//! revm asks four questions of the database:
//!   - `basic(address)`   → nonce, balance, code hash, (cached) code
//!   - `code_by_hash(h)`  → bytecode for a code hash
//!   - `storage(a, k)`    → SLOAD
//!   - `block_hash(n)`    → BLOCKHASH
//!
//! Each is delegated to Mazze's `vm::Context`, which is itself wired to
//! Mazze's `State` by the executor.
//!
//! Mazze deviations are encoded here, not by patching revm:
//!   - `block_hash` returns the previous-block hash for `number == env.number - 1`
//!     and zero otherwise. Mazze eSpace exposes only one lookback block;
//!     Ethereum allows 256.
//!
//! Writes do **not** go through this trait. revm collects them as a
//! `ResultAndState` at `Evm::transact()` time, and the `RevmExec::exec`
//! adapter (in `lib.rs`) walks that diff and applies each change to
//! `vm::Context` via the by-address state methods we added to the trait.

use alloy_primitives::U256 as AU256;
use log::trace;
use mazze_types::{H256, U256};
use mazze_vm_types::Context as MazzeContext;
use revm::bytecode::Bytecode;
use revm::primitives::{Address, KECCAK_EMPTY, B256};
use revm::state::AccountInfo;
use revm::Database;
use std::collections::HashMap;
use std::convert::Infallible;

/// Bridges revm's `Database` trait to Mazze's `vm::Context`.
///
/// Holds a mutable reference because `Database::basic` and `Database::storage`
/// take `&mut self`, and Mazze's `Context` storage reads also take `&mut self`
/// (cache fill-ins as a side effect).
///
/// Maintains an in-process cache mapping `code_hash` → `bytecode` populated
/// by `basic()`. revm calls `code_by_hash()` only after a prior `basic()`
/// already returned the same hash, so the cache hit rate is effectively
/// 100% in practice — but the code path is defensive for the rare miss.
pub struct MazzeDatabase<'ctx> {
    ctx: &'ctx mut dyn MazzeContext,
    /// Cache from code_hash → bytecode. Populated on every `basic()` call
    /// that finds a non-empty code account, so subsequent `code_by_hash`
    /// queries for the same hash are O(1).
    code_cache: HashMap<B256, Bytecode>,
    /// The tx sender's address, used to compensate for Mazze's
    /// pre-VM nonce increment. See `basic()` for the rationale.
    sender: mazze_types::Address,
    /// For a CREATE tx, the pre-computed target contract address. Mazze
    /// initialises a "shell" account there (nonce=1, empty code, balance =
    /// tx value) *before* the VM runs — see
    /// `transfer_exec_balance_and_init_contract`. revm would then trip
    /// `CreateCollision` on the same address. `basic()` returns `None`
    /// for this address so revm sees a clean slot. `None` for CALL txs.
    create_target: Option<mazze_types::Address>,
}

impl<'ctx> MazzeDatabase<'ctx> {
    pub fn new(
        ctx: &'ctx mut dyn MazzeContext, sender: mazze_types::Address,
        create_target: Option<mazze_types::Address>,
    ) -> Self {
        Self {
            ctx,
            code_cache: HashMap::new(),
            sender,
            create_target,
        }
    }
}

/// Convert alloy/revm `Address` (20 bytes) to Mazze `Address`.
#[inline]
fn to_mazze_address(a: Address) -> mazze_types::Address {
    mazze_types::Address::from_slice(a.as_slice())
}

/// Convert Mazze `U256` to alloy/revm `U256` via big-endian bytes.
#[inline]
fn to_alloy_u256(v: U256) -> AU256 {
    let mut be = [0u8; 32];
    v.to_big_endian(&mut be);
    AU256::from_be_bytes(be)
}

/// Convert Mazze `H256` to alloy/revm `B256`.
#[inline]
fn to_b256(h: H256) -> B256 {
    B256::from(h.0)
}

impl<'ctx> Database for MazzeDatabase<'ctx> {
    /// The database is infallible from revm's perspective. Any real error
    /// reading Mazze state surfaces as a Mazze `vm::Error::Wasm(...)` at
    /// the by-address Context method, which the adapter translates to
    /// "account not found" → `Ok(None)` for revm. Real DB failures bubble
    /// up via the executor's normal DbError plumbing.
    type Error = Infallible;

    fn basic(
        &mut self, address: Address,
    ) -> Result<Option<AccountInfo>, Self::Error> {
        let addr = to_mazze_address(address);

        // For a CREATE tx, Mazze pre-creates a "shell" contract account
        // at the target CREATE address with nonce=1 and empty code (see
        // [stack/frame_start.rs::transfer_exec_balance_and_init_contract]
        // and [overlay_account/factory.rs::new_contract_with_admin]).
        // revm would then trip Halt(CreateCollision) on the same
        // address because (nonce != 0 OR code != empty) → collision per
        // EIP-684.
        //
        // We can't hide the shell entirely (that breaks Mazze's commit
        // path — the OverlayAccount needs to stay in cache so the
        // post-VM state-diff write updates *this* entry, which then
        // commits). Instead, lie that the account is EIP-161 empty —
        // nonce=0, balance=0, empty code. revm allows CREATE on an
        // empty account, and our apply_state_diff updates the shell in
        // cache afterwards with the new nonce + code from revm's diff.
        if Some(addr) == self.create_target {
            return Ok(Some(AccountInfo {
                balance: AU256::ZERO,
                nonce: 0,
                code_hash: KECCAK_EMPTY,
                account_id: None,
                code: None,
            }));
        }

        // exists() returns false for unknown accounts. revm treats Ok(None)
        // as "account does not exist".
        let exists = self.ctx.exists(&addr).unwrap_or(false);
        if !exists {
            return Ok(None);
        }

        // Mazze pre-increments the tx sender's nonce *before* the VM runs
        // (PreCheckedExecutive::inc_sender_nonce). revm 40 then increments
        // again as part of its own tx-processing flow, so without this
        // adjustment revm would see the sender at N+1 (vs. the N that
        // signed the tx) and derive every CREATE address from N+1. To
        // keep CREATE addresses identical to what Mazze's receipt layer
        // pre-computed from the original tx nonce, subtract one for the
        // sender's nonce here so revm sees both `tx.nonce` and
        // `state.sender.nonce == N`. revm will then increment to N+1
        // — which is exactly the post-VM state Mazze already has.
        let mut nonce = self.ctx.nonce_of(&addr).unwrap_or_default();
        if addr == self.sender && nonce > U256::zero() {
            nonce = nonce - U256::one();
        }
        // Saturate before u64 narrowing — Mazze stores nonces in U256
        // and the `pre.nonce = u64::MAX` state-test path produces a
        // raw value above u64 range until the subtract-1 above.
        let nonce_u64 = if nonce > U256::from(u64::MAX) {
            u64::MAX
        } else {
            nonce.as_u64()
        };
        let balance = self.ctx.balance(&addr).unwrap_or_default();
        let code_hash =
            self.ctx.extcodehash(&addr).unwrap_or(H256::zero());

        // Fetch code so we can populate the cache and hand revm a Bytecode
        // immediately (saves a code_by_hash round-trip).
        let code_opt = self.ctx.extcode(&addr).unwrap_or(None);
        let code = code_opt.map(|c| Bytecode::new_raw(c.to_vec().into()));

        let code_b256 = if code_hash == H256::zero() {
            KECCAK_EMPTY
        } else {
            to_b256(code_hash)
        };

        // EIP-161 / EIP-684: an account with nonce=0, code=empty (and
        // here also balance=0) is considered non-existent for the
        // purpose of CREATE collision detection. Mazze's executor
        // pre-creates the contract account at the target CREATE address
        // *before* the VM runs (see
        // `transfer_exec_balance_and_init_contract` in
        // `executor/src/stack/frame_start.rs`), which would otherwise
        // trip revm's CreateCollision check. Return None for these
        // "ghost" accounts so revm treats the slot as deployable.
        let code_is_empty = code_b256 == KECCAK_EMPTY;
        if nonce.is_zero() && balance.is_zero() && code_is_empty {
            return Ok(None);
        }

        if let Some(ref bc) = code {
            self.code_cache.insert(code_b256, bc.clone());
        }

        Ok(Some(AccountInfo {
            balance: to_alloy_u256(balance),
            nonce: nonce_u64,
            code_hash: code_b256,
            // account_id is a revm-internal optimization hint; None forces
            // revm to look up storage by address each time. Safe default.
            account_id: None,
            code,
        }))
    }

    fn code_by_hash(
        &mut self, code_hash: B256,
    ) -> Result<Bytecode, Self::Error> {
        if code_hash == KECCAK_EMPTY {
            return Ok(Bytecode::new());
        }
        if let Some(bc) = self.code_cache.get(&code_hash) {
            return Ok(bc.clone());
        }
        // Cache miss: in practice this should not happen because revm only
        // asks for a code hash it learned from a prior basic() call. If it
        // does happen, return empty code (the contract will halt on
        // execution; safer than panicking).
        trace!(
            "mazze-eth-vm: code_by_hash miss for {:?}; returning empty",
            code_hash
        );
        Ok(Bytecode::new())
    }

    fn storage(
        &mut self, address: Address, index: AU256,
    ) -> Result<AU256, Self::Error> {
        let addr = to_mazze_address(address);
        // Mazze stores storage keyed by raw 32-byte big-endian U256 slot
        // index — same as Ethereum.
        let key: [u8; 32] = index.to_be_bytes();
        let value = self
            .ctx
            .storage_at_address(&addr, &key)
            .unwrap_or_default();
        Ok(to_alloy_u256(value))
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        // Mazze eSpace deviation: only the previous block is exposed.
        // Ethereum allows 256-block lookback; we return zero for anything
        // outside the [current-1, current-1] window. This matches Ethereum
        // semantics for out-of-window requests (zero).
        let current = self.ctx.env().number;
        if current > 0 && number == current - 1 {
            // env.last_hash carries the previous block's hash.
            return Ok(to_b256(self.ctx.env().last_hash));
        }
        Ok(B256::ZERO)
    }
}
