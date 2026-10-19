// Copyright 2026 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Execution tests for the `0x0888…` internal-contract registry.
//!
//! Each test builds a real `State` + `Machine` + `InternalRefContext`,
//! ABI-encodes a call, and dispatches via
//! `InternalContractTrait::execute` so the test exercises the full
//! ABI-decode → `SimpleExecutionTrait::execute_inner` → state-mutation
//! pipeline that an external caller hits, without the outer
//! `Executable::execute` frame loop.
//!
//! Run with:
//!
//! ```bash
//! cargo test -p mazze-executor --test internal_contracts
//! ```

use keccak_hash::keccak;
use mazze_executor::{
    executive_observer::TracerTrait,
    internal_contract::{
        initialize_internal_contract_accounts, InternalContractTrait,
        InternalRefContext, InternalTrapResult,
    },
    machine::{new_machine_with_builtin, Machine, VmFactory},
    stack::CallStackInfo,
    state::{State, StateCommitResult},
    substate::Substate,
};
use mazze_parameters::internal_contract_addresses::{
    ADMIN_CONTROL_CONTRACT_ADDRESS,
    SPONSOR_WHITELIST_CONTROL_CONTRACT_ADDRESS,
};
use mazze_statedb::StateDb;
use mazze_storage::{
    state_manager::StateManagerTrait,
    tests::{new_state_manager_for_unit_test, FakeStateManager},
};
use mazze_types::{
    address_util::AddressUtil, Address, AddressSpaceUtil, Space, U256,
};
use mazze_vm_types::{
    ActionParams, ActionValue, CallType, CreateType, Env, ParamsType, Spec,
};
use primitives::storage::STORAGE_LAYOUT_REGULAR_V0;
use solidity_abi::ABIEncodable;
use std::sync::Arc;

// ============================================================
// Harness
// ============================================================

/// Owns every value an `InternalRefContext` needs to live. Tests pull
/// a mutable context out of this via `dispatch()`. Kept self-contained
/// so each test can spin up an isolated state without sharing.
struct Harness {
    // Owns the temp directory + storage backend for the test's lifetime.
    // Dropping this on test exit cleans up the on-disk state.
    _storage_manager: FakeStateManager,
    state: State,
    machine: Machine,
    env: Env,
    substate: Substate,
    callstack: CallStackInfo,
    tracer: (),
}

impl Harness {
    fn new() -> Self {
        let storage_manager = new_state_manager_for_unit_test();
        let machine = new_machine_with_builtin(
            Default::default(),
            VmFactory::new(1024 * 32),
        );

        // Mirror `state_object::tests::get_state_for_genesis_write`,
        // which is `#[cfg(test)]`-gated and not visible to integration
        // tests. We inline the same setup steps here so the harness is
        // self-contained.
        let state = {
            let mut state = State::new(StateDb::new(
                storage_manager.get_state_for_genesis_write(),
            ))
            .expect("State::new (initial) failed");
            let addresses: Vec<Address> = machine
                .internal_contracts()
                .keys()
                .copied()
                .collect();
            initialize_internal_contract_accounts(&mut state, &addresses)
                .expect("initialize_internal_contract_accounts failed");
            // Don't commit + re-open — committing prunes empty accounts
            // (zero balance, no code), which would drop the internal
            // contract stubs we just created. Use the uncommitted state
            // directly. This is fine for unit tests, which never
            // round-trip through commit.
            state
        };

        Self {
            _storage_manager: storage_manager,
            state,
            machine,
            env: Env::default(),
            substate: Substate::new(),
            callstack: CallStackInfo::new(),
            tracer: (),
        }
    }

    fn spec(&self) -> Spec {
        self.machine.spec(self.env.number, self.env.epoch_height)
    }
}

/// Verifies the StateCommitResult is something we can compare against.
/// Forces the import to be used even on tests that don't read commits.
#[allow(dead_code)]
fn _ensure_state_commit_is_typed(_r: StateCommitResult) {}

/// Build an `InternalRefContext` from a `Harness` and dispatch the
/// internal contract at `contract_address`. The callback receives the
/// contract reference + params + a fully-formed ref context.
fn dispatch<R>(
    harness: &mut Harness,
    spec: &Spec,
    contract_address: &Address,
    params: ActionParams,
    f: impl FnOnce(
        &Box<dyn InternalContractTrait>,
        &ActionParams,
        &mut InternalRefContext,
    ) -> R,
) -> R {
    let contract = harness
        .machine
        .internal_contracts()
        .contract(&contract_address.with_native_space(), spec)
        .expect("internal contract should be active at genesis");
    // The contract reference borrows from `machine`; mutating the
    // harness's other fields requires we hold the contract reference
    // separately from the mutable borrows that build the
    // InternalRefContext.
    let Harness {
        state,
        substate,
        callstack,
        tracer,
        env,
        ..
    } = harness;
    let mut ref_ctx = InternalRefContext {
        env,
        spec,
        callstack,
        state,
        substate,
        tracer: tracer as &mut dyn TracerTrait,
        static_flag: false,
        depth: 0,
    };
    f(contract, &params, &mut ref_ctx)
}

/// Build a call-style `ActionParams` for an internal-contract invocation.
fn make_call_params(
    sender: Address, contract: Address, gas: u64, data: Vec<u8>,
) -> ActionParams {
    ActionParams {
        space: Space::Native,
        sender,
        address: contract,
        code_address: contract,
        original_sender: sender,
        storage_owner: sender,
        gas: U256::from(gas),
        gas_price: U256::zero(),
        value: ActionValue::Transfer(U256::zero()),
        code: None,
        code_hash: Default::default(),
        data: Some(data),
        call_type: CallType::Call,
        create_type: CreateType::None,
        params_type: ParamsType::Embedded,
    }
}

/// `keccak256(sig)[..4]` — the four-byte solidity selector.
fn selector(sig: &str) -> [u8; 4] {
    let h = keccak(sig.as_bytes());
    let mut out = [0u8; 4];
    out.copy_from_slice(&h.0[..4]);
    out
}

/// Encode a call's calldata as `selector(sig) || abi_encode(args)`.
fn encode_call<T: ABIEncodable>(sig: &str, args: &T) -> Vec<u8> {
    let mut data = Vec::with_capacity(4 + 64);
    data.extend_from_slice(&selector(sig));
    data.extend_from_slice(&args.abi_encode());
    data
}

/// Helper for making a contract address (high nibble `0x80`).
fn random_contract_addr() -> Address {
    let mut addr = Address::random();
    addr.set_contract_type_bits();
    addr
}

/// Helper for making a user-account address (high nibble `0x10`).
fn random_user_addr() -> Address {
    let mut addr = Address::random();
    addr.set_user_account_type_bits();
    addr
}

fn create_contract_account(
    harness: &mut Harness, contract: Address, admin: Address,
) {
    let contract_with_space = contract.with_native_space();
    harness
        .state
        .new_contract_with_admin(
            &contract_with_space,
            &admin,
            U256::zero(),
            Some(STORAGE_LAYOUT_REGULAR_V0),
        )
        .expect("create contract failed");
    // Give the contract empty code so `is_contract_address` returns true.
    harness
        .state
        .init_code(&contract_with_space, vec![0u8; 1], admin)
        .expect("init_code failed");
}

// ============================================================
// AdminControl
// ============================================================

#[test]
fn admin_get_returns_creation_admin() {
    let mut harness = Harness::new();
    let admin = random_user_addr();
    let contract = random_contract_addr();
    create_contract_account(&mut harness, contract, admin);

    let spec = harness.spec();
    let data = encode_call("getAdmin(address)", &contract);
    let caller = random_user_addr();
    let params = make_call_params(
        caller,
        ADMIN_CONTROL_CONTRACT_ADDRESS,
        100_000,
        data,
    );

    let result = dispatch(
        &mut harness,
        &spec,
        &ADMIN_CONTROL_CONTRACT_ADDRESS,
        params,
        |contract_box, params, ref_ctx| contract_box.execute(params, ref_ctx),
    );

    assert!(matches!(result, InternalTrapResult::Return(Ok(_))));
    assert_eq!(harness.state.admin(&contract).unwrap(), admin);
}

#[test]
fn admin_set_by_admin_succeeds() {
    let mut harness = Harness::new();
    let old_admin = random_user_addr();
    let new_admin = random_user_addr();
    let contract = random_contract_addr();
    create_contract_account(&mut harness, contract, old_admin);

    let spec = harness.spec();
    let data = encode_call(
        "setAdmin(address,address)",
        &(contract, new_admin),
    );
    let params = make_call_params(
        old_admin, // sender == current admin
        ADMIN_CONTROL_CONTRACT_ADDRESS,
        100_000,
        data,
    );

    let result = dispatch(
        &mut harness,
        &spec,
        &ADMIN_CONTROL_CONTRACT_ADDRESS,
        params,
        |contract_box, params, ref_ctx| contract_box.execute(params, ref_ctx),
    );

    assert!(matches!(result, InternalTrapResult::Return(Ok(_))));
    assert_eq!(
        harness.state.admin(&contract).unwrap(),
        new_admin,
        "setAdmin from current admin must update state",
    );
}

#[test]
fn admin_set_by_non_admin_is_no_op() {
    let mut harness = Harness::new();
    let old_admin = random_user_addr();
    let attacker = random_user_addr();
    let target_new_admin = random_user_addr();
    let contract = random_contract_addr();
    create_contract_account(&mut harness, contract, old_admin);

    let spec = harness.spec();
    let data = encode_call(
        "setAdmin(address,address)",
        &(contract, target_new_admin),
    );
    let params = make_call_params(
        attacker, // sender != admin
        ADMIN_CONTROL_CONTRACT_ADDRESS,
        100_000,
        data,
    );

    let result = dispatch(
        &mut harness,
        &spec,
        &ADMIN_CONTROL_CONTRACT_ADDRESS,
        params,
        |contract_box, params, ref_ctx| contract_box.execute(params, ref_ctx),
    );

    // The contract does NOT revert on unauthorized setAdmin — it just
    // silently no-ops (see admin.rs::set_admin). This lock-in test
    // makes that quirk explicit: a future maintainer who decides to
    // make unauthorized calls revert will see this assertion break.
    assert!(matches!(result, InternalTrapResult::Return(Ok(_))));
    assert_eq!(
        harness.state.admin(&contract).unwrap(),
        old_admin,
        "non-admin setAdmin must be a no-op, NOT a revert",
    );
}

// ============================================================
// SponsorWhitelistControl
// ============================================================

#[test]
fn sponsor_whitelist_add_then_remove_round_trip() {
    let mut harness = Harness::new();
    let admin = random_user_addr();
    let user_a = random_user_addr();
    let user_b = random_user_addr();
    let contract = random_contract_addr();
    create_contract_account(&mut harness, contract, admin);

    let spec = harness.spec();

    // addPrivilege must be called from the contract itself (sender ==
    // the contract whose whitelist is being modified). Anything else
    // is a silent no-op.
    let data =
        encode_call("addPrivilege(address[])", &vec![user_a, user_b]);
    let params = make_call_params(
        contract,
        SPONSOR_WHITELIST_CONTROL_CONTRACT_ADDRESS,
        200_000,
        data,
    );

    let result = dispatch(
        &mut harness,
        &spec,
        &SPONSOR_WHITELIST_CONTROL_CONTRACT_ADDRESS,
        params,
        |contract_box, params, ref_ctx| contract_box.execute(params, ref_ctx),
    );

    assert!(
        matches!(result, InternalTrapResult::Return(Ok(_))),
        "addPrivilege should succeed when called from the contract itself",
    );
    assert!(
        harness
            .state
            .check_contract_whitelist(&contract, &user_a)
            .unwrap(),
        "user_a should be whitelisted after addPrivilege",
    );
    assert!(
        harness
            .state
            .check_contract_whitelist(&contract, &user_b)
            .unwrap(),
        "user_b should be whitelisted after addPrivilege",
    );

    // Now remove just user_a.
    let data = encode_call("removePrivilege(address[])", &vec![user_a]);
    let params = make_call_params(
        contract,
        SPONSOR_WHITELIST_CONTROL_CONTRACT_ADDRESS,
        200_000,
        data,
    );
    let result = dispatch(
        &mut harness,
        &spec,
        &SPONSOR_WHITELIST_CONTROL_CONTRACT_ADDRESS,
        params,
        |contract_box, params, ref_ctx| contract_box.execute(params, ref_ctx),
    );
    assert!(matches!(result, InternalTrapResult::Return(Ok(_))));
    assert!(
        !harness
            .state
            .check_contract_whitelist(&contract, &user_a)
            .unwrap(),
        "user_a should be removed",
    );
    assert!(
        harness
            .state
            .check_contract_whitelist(&contract, &user_b)
            .unwrap(),
        "user_b should remain whitelisted",
    );
}

#[test]
fn sponsor_whitelist_add_by_non_contract_is_no_op() {
    // addPrivilege from a non-contract sender silently no-ops — same
    // "no revert on auth failure" pattern as setAdmin.
    let mut harness = Harness::new();
    let admin = random_user_addr();
    let attacker = random_user_addr();
    let victim_user = random_user_addr();
    let contract = random_contract_addr();
    create_contract_account(&mut harness, contract, admin);

    let spec = harness.spec();
    let data = encode_call("addPrivilege(address[])", &vec![victim_user]);
    let params = make_call_params(
        attacker, // sender != contract
        SPONSOR_WHITELIST_CONTROL_CONTRACT_ADDRESS,
        200_000,
        data,
    );

    let _ = dispatch(
        &mut harness,
        &spec,
        &SPONSOR_WHITELIST_CONTROL_CONTRACT_ADDRESS,
        params,
        |contract_box, params, ref_ctx| contract_box.execute(params, ref_ctx),
    );

    assert!(
        !harness
            .state
            .check_contract_whitelist(&contract, &victim_user)
            .unwrap(),
        "addPrivilege from a non-contract sender must NOT touch the whitelist",
    );
}

// ============================================================
// SystemStorage substrate
// ============================================================

#[test]
fn system_storage_state_roundtrip() {
    // The SystemStorage internal contract sits on top of
    // `State::get_system_storage` / `set_system_storage`. These tests
    // lock in the substrate's determinism + persistence semantics
    // independently of the ABI dispatch.
    let mut harness = Harness::new();

    let key = b"test:sys_storage:k1".to_vec();
    let value = U256::from(0xDEADBEEFu64);

    harness
        .state
        .set_system_storage(key.clone(), value)
        .unwrap();

    let readback = harness.state.get_system_storage(&key).unwrap();
    assert_eq!(readback, value, "system storage round-trip failed");

    // Overwrite, then read again.
    let value2 = U256::from(0xC0FFEEu64);
    harness
        .state
        .set_system_storage(key.clone(), value2)
        .unwrap();
    assert_eq!(
        harness.state.get_system_storage(&key).unwrap(),
        value2,
        "system storage overwrite failed",
    );

    // A different key must read back zero (unset).
    let other_key = b"test:sys_storage:k2".to_vec();
    assert_eq!(
        harness.state.get_system_storage(&other_key).unwrap(),
        U256::zero(),
        "unset system storage key must read back zero",
    );
}
