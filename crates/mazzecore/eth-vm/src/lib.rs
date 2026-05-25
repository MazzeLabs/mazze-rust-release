// Copyright 2025 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! # mazze-eth-vm
//!
//! revm-backed EVM for the Mazze eSpace.
//!
//! ## Why this crate exists
//!
//! The native space EVM ([crates/mazzecore/vm-interpreter](../mazze_vm_interpreter))
//! is hand-maintained Parity-era code. Keeping it in step with mainline
//! Ethereum (PUSH0, MCOPY, TLOAD/TSTORE, BLOBHASH, Prague EIPs) requires
//! hand-porting every EIP. This crate replaces the eSpace VM with
//! [revm](https://github.com/bluealloy/revm), which already tracks mainline
//! Ethereum, so future fork upgrades become a `cargo update revm` plus a
//! bump of `ESPACE_SPEC` in `spec_map.rs`.
//!
//! ## Boundaries
//!
//! - **eSpace**: routes here, via `RevmExec`.
//! - **Native space**: continues to route through `mazze-vm-interpreter`.
//! - **Cross-space bridging**: lives in the *internal contract* layer
//!   (`crates/mazzecore/executor/src/internal_contract/contracts/cross_space.rs`),
//!   which is dispatched in `make_executable()` **before** the VM factory is
//!   called. revm never sees cross-space calls; the bridge is invisible to
//!   it. This crate must therefore not concern itself with cross-space
//!   semantics — they are upstream of the VM.
//! - **Sponsor mechanism**: lives in `PreCheckedExecutive::charge_gas` (pre-VM).
//! - **Storage collateral**: lives in `PreCheckedExecutive::settle_collateral`
//!   (post-VM).
//!
//! revm sees normalised gas and normalised balances. All Mazze-specific
//! economics happen at the executor layer.
//!
//! ## Architecture of `RevmExec::exec`
//!
//! Each invocation does:
//!
//! 1. Build a `MazzeDatabase` adapter that wraps the incoming `&mut dyn
//!    vm::Context` and delegates state reads to it (see `database.rs`).
//! 2. Construct a revm `Context::mainnet()`, set the spec to
//!    [`ESPACE_SPEC`](spec_map::ESPACE_SPEC), wire the database, and set the
//!    block environment from Mazze's `Env`.
//! 3. Build a `MainnetEvm` and call `transact(tx_env)`.
//! 4. Walk the returned [`EvmState`](revm::state::EvmState) diff and apply
//!    every account change back to `vm::Context` via the by-address state
//!    methods (`set_balance`, `set_nonce`, `set_code`,
//!    `set_storage_at_address`).
//! 5. Replay logs through `vm::Context::log_for_address`.
//! 6. Map the revm `ExecutionResult` variant to Mazze's `GasLeft` / `Error`.
//!
//! ## SELFDESTRUCT
//!
//! `apply_state_diff` calls `vm::Context::suicide_for_address`, which
//! pushes the dead contract into `substate.suicides`. The executor's
//! post-VM cleanup (pre_checked_executive.rs ~431) then burns leftover
//! accounting and calls `state.remove_contract`. Balance transfers to the
//! refund target are already in revm's EvmState diff, so we don't
//! double-move them here.
//!
//! ## Known integration gap (single remaining item)
//!
//! - Mazze internal contracts (cross-space, sponsor, admin, system-storage)
//!   called *from inside* an eSpace contract via `CALL 0x88880…` need to
//!   be wired as revm custom precompiles. Today they reach revm as
//!   ordinary external addresses with no code; the call returns empty
//!   data. **As tx-level entry points they still work** because Mazze's
//!   `make_executable()` dispatches them before the VM factory. Only the
//!   `eSpace contract → internal contract` path is affected. Wiring this
//!   needs the `revm::Inspector` + `MainBuilder::with_precompiles` pattern.

pub mod database;
pub mod spec_map;

pub use database::MazzeDatabase;
pub use spec_map::{revm_spec_for, ESPACE_SPEC};

use alloy_primitives::{Address as AAddress, Bytes as ABytes, U256 as AU256};
use log::debug;
use mazze_types::{Address, Space, H256, U256};
use mazze_vm_types::{
    ActionParams, ActionValue, CallType, Context as MazzeContext,
    Error as VmError, ExecTrapResult, GasLeft, ReturnData, Spec, TrapResult,
};
use revm::context::result::{ExecutionResult, Output};
use revm::context::{BlockEnv, TxEnv};
use revm::context_interface::block::BlobExcessGasAndPrice;
use revm::primitives::TxKind;
use revm::state::EvmState;
use revm::{Context, ExecuteEvm, MainBuilder, MainContext};

/// Adapter that implements Mazze's `vm::Exec` trait by driving a revm
/// `MainnetEvm` over [`MazzeDatabase`].
pub struct RevmExec {
    params: ActionParams,
    gas: U256,
}

impl RevmExec {
    pub fn new(params: ActionParams, gas: U256, _spec: &Spec) -> Self {
        Self { params, gas }
    }
}

impl mazze_vm_types::Exec for RevmExec {
    fn exec(
        self: Box<Self>, ctx: &mut dyn MazzeContext,
    ) -> ExecTrapResult<GasLeft> {
        match run_revm(*self, ctx) {
            Ok(gas_left) => TrapResult::Return(Ok(gas_left)),
            Err(e) => TrapResult::Return(Err(e)),
        }
    }
}

fn run_revm(
    exec: RevmExec, ctx: &mut dyn MazzeContext,
) -> Result<GasLeft, VmError> {
    let RevmExec { params, gas } = exec;
    let env = ctx.env();
    let block_number = env.number;
    let chain_id = ctx.chain_id();

    // Snapshot what we need from `env` before we hand `ctx` to the database.
    // eSpace base fee comes from CIP-1559's per-space `base_gas_price`.
    // revm models basefee as u64 — saturating is safe because Mazze's
    // base_gas_price is bounded well below u64::MAX in any realistic
    // configuration.
    let base_fee = env.base_gas_price[Space::Ethereum];
    let basefee_u64 = if base_fee > U256::from(u64::MAX) {
        u64::MAX
    } else {
        base_fee.as_u64()
    };
    let block_env = BlockEnv {
        number: AU256::from(env.number),
        timestamp: AU256::from(env.timestamp),
        gas_limit: env.gas_limit.as_u64(),
        beneficiary: to_alloy_address(env.author),
        difficulty: to_alloy_u256(env.difficulty),
        // Post-Merge: PREVRANDAO replaces DIFFICULTY. revm reads from
        // `prevrandao` for Shanghai+. Set to a deterministic value from
        // the block hash for now; Mazze does not have beacon-chain
        // randomness.
        prevrandao: Some(to_b256(env.last_hash)),
        basefee: basefee_u64,
        // Cancun+ requires this to be Some. Mazze does not implement
        // EIP-4844 blob transactions, so excess_blob_gas is always zero.
        // `new_with_spec` derives the blob-base-fee update fraction from
        // the spec id (PRAGUE differs from CANCUN).
        blob_excess_gas_and_price: Some(BlobExcessGasAndPrice::new_with_spec(
            0,
            revm_spec_for(block_number),
        )),
        // EIP-7251 / Pectra slot number. Mazze is PoW; no slot concept.
        // revm uses this for staking-related logic that we don't exercise.
        slot_num: 0,
    };

    // Mazze pre-increments the sender's nonce in
    // `PreCheckedExecutive::inc_sender_nonce` *before* the VM runs. revm
    // 40 expects the pre-increment value in both TxEnv.nonce and
    // state.sender.nonce. The MazzeDatabase adapter overrides
    // `basic(sender)` to return nonce-1 so revm sees consistent values
    // for both — see [database.rs]. Here we read what revm will see
    // (post the adapter's subtraction) and pass that to TxEnv.nonce.
    // Subtract in U256 first to handle the `pre.nonce = u64::MAX` test
    // case where after Mazze's bump the raw value is `u64::MAX + 1`
    // (representable in U256 but not u64); subtracting first keeps it
    // in u64 range. Saturate to `u64::MAX` if the result would still
    // exceed u64 — that lets revm fire its own nonce-overflow check.
    let caller_nonce = ctx
        .nonce_of(&params.sender)
        .map(|n| {
            let pre = if n > U256::zero() {
                n - U256::one()
            } else {
                U256::zero()
            };
            if pre > U256::from(u64::MAX) {
                u64::MAX
            } else {
                pre.as_u64()
            }
        })
        .unwrap_or(0);

    // Translate ActionParams → revm TxEnv. params.call_type tells us call
    // vs. create. ActionValue::Apparent (for DELEGATECALL etc.) does not
    // transfer value; revm handles that internally based on the call type
    // — for our top-level entry we treat value as transferred.
    let value = match params.value {
        ActionValue::Transfer(v) | ActionValue::Apparent(v) => v,
    };
    let is_create = matches!(params.call_type, CallType::None);
    let kind = if is_create {
        TxKind::Create
    } else {
        TxKind::Call(to_alloy_address(params.code_address))
    };
    // For CREATE Mazze passes the init code in `params.code`; the EVM
    // standard is to put the *full create input* (init code + appended
    // constructor args) in TxEnv.data. For CALL we just pass through
    // params.data.
    let data = if is_create {
        let init = params
            .code
            .as_ref()
            .map(|c| (**c).clone())
            .unwrap_or_default();
        let args = params.data.clone().unwrap_or_default();
        if args.is_empty() {
            ABytes::from(init)
        } else {
            let mut buf = init;
            buf.extend_from_slice(&args);
            ABytes::from(buf)
        }
    } else {
        params
            .data
            .as_ref()
            .map(|d| ABytes::from(d.clone()))
            .unwrap_or_default()
    };
    let gas_limit = gas.as_u64();
    let gas_price = u256_to_u128_saturating(params.gas_price);

    let tx_env = TxEnv::builder()
        .caller(to_alloy_address(params.sender))
        .gas_limit(gas_limit)
        .gas_price(gas_price)
        .kind(kind)
        .value(to_alloy_u256(value))
        .data(data)
        .nonce(caller_nonce)
        .chain_id(Some(chain_id))
        .build()
        .map_err(|e| {
            VmError::Wasm(format!("revm TxEnv build failed: {:?}", e))
        })?;

    // Build the EVM. The database borrows ctx mutably until we drop the
    // Evm. We capture the result state into an owned EvmState before the
    // scope ends so we can re-borrow ctx for state-diff application.
    let sender = params.sender;
    let create_target = if is_create {
        Some(params.code_address)
    } else {
        None
    };
    let (result, state) = {
        let db = MazzeDatabase::new(ctx, sender, create_target);
        let evm_ctx = Context::mainnet()
            .modify_cfg_chained(|cfg| {
                cfg.spec = revm_spec_for(block_number);
                cfg.chain_id = chain_id;
            })
            .modify_block_chained(|b| {
                *b = block_env;
            })
            .with_db(db);
        let mut evm = evm_ctx.build_mainnet();
        let res = evm.transact(tx_env).map_err(|e| {
            VmError::Wasm(format!("revm transact failed: {:?}", e))
        })?;
        (res.result, res.state)
    };
    // `ctx` is borrowable again here; the Evm and its database have been
    // dropped.

    apply_state_diff(ctx, state)?;
    apply_create_output(ctx, &result)?;
    replay_logs(ctx, &result)?;
    map_execution_result(result, gas_limit)
}

/// For successful CREATE transactions, write the deployed runtime code
/// to state. revm 40's `EvmState` diff puts the new account into
/// `is_created()` state with `info.code = None` (or an empty Bytecode
/// placeholder) — the actual runtime code lives in
/// `ExecutionResult::Success { output: Output::Create(bytes, addr), .. }`.
/// Without this pass, a CREATE leaves an account with the right balance
/// and nonce but no bytecode, so subsequent calls find an empty address.
fn apply_create_output(
    ctx: &mut dyn MazzeContext, result: &ExecutionResult,
) -> Result<(), VmError> {
    if let ExecutionResult::Success { output, .. } = result {
        if let Output::Create(bytes, Some(addr)) = output {
            if !bytes.is_empty() {
                let m_addr = to_mazze_address(*addr);
                debug!(
                    "revm CREATE deployed {} bytes at {:?}",
                    bytes.len(),
                    m_addr
                );
                ctx.set_code(&m_addr, bytes.to_vec())?;
            }
        }
    }
    Ok(())
}

// -------------------------------------------------------------------------
// State-diff application.
// -------------------------------------------------------------------------

fn apply_state_diff(
    ctx: &mut dyn MazzeContext, state: EvmState,
) -> Result<(), VmError> {
    // First pass: apply absolute balance/nonce/code/storage. revm has
    // already accounted for SELFDESTRUCT balance transfers in the diff —
    // both the selfdestructed contract's zeroed balance and the refund
    // target's bumped balance appear here as ordinary writes.
    //
    // Second pass: register each selfdestruct in `substate.suicides` so
    // the executor's post-VM cleanup (pre_checked_executive ~431) removes
    // the dead account from state. We can't do this inline because the
    // refund target's balance update may come in a separate iteration of
    // the EvmState map.
    let mut suicides: Vec<Address> = Vec::new();

    for (addr, account) in state {
        if !account.is_touched() {
            // revm marks accounts touched whenever they participate in a
            // state change. Skip untouched accounts to avoid spurious
            // writes.
            continue;
        }
        let m_addr = to_mazze_address(addr);
        // Capture the selfdestruct flag before we partially move `account`
        // by iterating its storage.
        let dead = account.is_selfdestructed();

        // Balance & nonce: always re-set to the post-tx absolute values.
        // For a selfdestructed account, revm gives us a zeroed balance;
        // for the refund target, revm gives us the bumped balance. Both
        // are correct here.
        ctx.set_balance(&m_addr, to_mazze_u256(account.info.balance))?;
        ctx.set_nonce(&m_addr, U256::from(account.info.nonce))?;

        // New code (after CREATE / CREATE2): write it. We detect new code
        // by the presence of a non-default Bytecode in the account info.
        if let Some(code) = account.info.code.as_ref() {
            let bytes = code.original_byte_slice();
            if !bytes.is_empty() {
                ctx.set_code(&m_addr, bytes.to_vec())?;
            }
        }

        // Storage diff.
        for (key, slot) in account.storage {
            if slot.present_value == slot.original_value {
                continue;
            }
            // alloy U256 → big-endian 32 bytes; this is the storage key
            // layout Mazze State uses for eSpace (matches Ethereum).
            let key_be: [u8; 32] = key.to_be_bytes();
            ctx.set_storage_at_address(
                &m_addr,
                key_be.to_vec(),
                to_mazze_u256(slot.present_value),
            )?;
        }

        if dead {
            suicides.push(m_addr);
        }
    }

    // Register suicides for post-tx cleanup. The refund target address is
    // not meaningfully used by `suicide_for_address` (revm already moved
    // the balance), so we pass zero — the implementation only needs the
    // contract address to insert into `substate.suicides`.
    for contract in suicides {
        ctx.suicide_for_address(&contract, &Address::zero())?;
    }
    Ok(())
}

// -------------------------------------------------------------------------
// Log replay.
// -------------------------------------------------------------------------

fn replay_logs(
    ctx: &mut dyn MazzeContext, result: &ExecutionResult,
) -> Result<(), VmError> {
    for log in result.logs() {
        let addr = to_mazze_address(log.address);
        let topics: Vec<H256> = log
            .data
            .topics()
            .iter()
            .map(|t| H256::from(t.0))
            .collect();
        ctx.log_for_address(&addr, topics, &log.data.data)?;
    }
    Ok(())
}

// -------------------------------------------------------------------------
// Result mapping.
// -------------------------------------------------------------------------

fn map_execution_result(
    result: ExecutionResult, gas_limit: u64,
) -> Result<GasLeft, VmError> {
    match result {
        ExecutionResult::Success {
            gas, output, ..
        } => {
            let used = gas.tx_gas_used();
            let gas_left = U256::from(gas_limit.saturating_sub(used));
            let data = match output {
                Output::Call(bytes) => bytes.to_vec(),
                Output::Create(bytes, _addr) => bytes.to_vec(),
            };
            // ReturnData is a (mem, offset, size) view; size MUST be the byte
            // length, not 0. With size 0 the data derefs to an empty slice,
            // which breaks eth_call return values and (for top-level CREATE)
            // fed the executor's create-finalization an empty body.
            let len = data.len();
            let return_data = ReturnData::new(data, 0, len);
            Ok(GasLeft::NeedsReturn {
                gas_left,
                data: return_data,
                apply_state: true,
            })
        }
        ExecutionResult::Revert { gas, output, .. } => {
            let used = gas.tx_gas_used();
            let gas_left = U256::from(gas_limit.saturating_sub(used));
            // size MUST be the byte length (see Success branch); the revert
            // payload carries the Solidity error/revert reason back to callers.
            let out = output.to_vec();
            let out_len = out.len();
            let return_data = ReturnData::new(out, 0, out_len);
            // REVERT: return data flows back to the caller but state is
            // rolled back. revm has already rolled back state changes for
            // the reverted frame; our state diff above contains the
            // pre-revert (caller's) state.
            debug!("revm: tx reverted, gas_used={}", used);
            Ok(GasLeft::NeedsReturn {
                gas_left,
                data: return_data,
                apply_state: false,
            })
        }
        ExecutionResult::Halt { reason, gas, .. } => {
            let used = gas.tx_gas_used();
            debug!(
                "revm: tx halted: reason={:?}, gas_used={}",
                reason, used
            );
            // Halt is a normal Ethereum outcome — the transaction
            // completes, all gas is consumed, state changes are reverted.
            // It is NOT an internal VM error. We model it as
            // NeedsReturn with no remaining gas and apply_state=false so
            // the Mazze executor sees the same semantics as `Revert`,
            // just without any return data.
            //
            // Halt covers: OutOfGas, InvalidJump, StackUnderflow, BadInstruction,
            // ReentrancySentryOOG, etc. — every EVM-level abort.
            let _ = reason;
            Ok(GasLeft::NeedsReturn {
                gas_left: U256::zero(),
                data: ReturnData::new(Vec::new(), 0, 0),
                apply_state: false,
            })
        }
    }
}

// -------------------------------------------------------------------------
// Type conversions between Mazze and alloy/revm.
// -------------------------------------------------------------------------

#[inline]
fn to_alloy_address(a: Address) -> AAddress {
    AAddress::from_slice(a.as_bytes())
}

#[inline]
fn to_mazze_address(a: AAddress) -> Address {
    Address::from_slice(a.as_slice())
}

#[inline]
fn to_alloy_u256(v: U256) -> AU256 {
    let mut be = [0u8; 32];
    v.to_big_endian(&mut be);
    AU256::from_be_bytes(be)
}

#[inline]
fn to_mazze_u256(v: AU256) -> U256 {
    let be: [u8; 32] = v.to_be_bytes();
    U256::from_big_endian(&be)
}

#[inline]
fn to_b256(h: H256) -> revm::primitives::B256 {
    revm::primitives::B256::from(h.0)
}

/// Saturating conversion from Mazze `U256` to revm `u128` gas_price.
/// Gas prices above `u128::MAX` are effectively impossible on a real chain
/// and silently saturate.
#[inline]
fn u256_to_u128_saturating(v: U256) -> u128 {
    if v > U256::from(u128::MAX) {
        u128::MAX
    } else {
        v.as_u128()
    }
}
