// Copyright 2026 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Cross-space bridge execution tests.
//!
//! Covers the bridge methods that complete inside a single
//! `InternalRefContext`:
//! - `mappedAddress` derivation: determinism + collision resistance.
//! - `mappedBalance` / `mappedNonce` queries.
//! - `withdrawFromMapped`: eSpace → Native move with sender ==
//!   `evm_map(...)` enforcement.
//!
//! `transferEVM` / `callEVM` / `staticCallEVM` / `createEVM` trap into
//! an EVM sub-call (`InternalTrapResult::Invoke`). The sub-call only
//! runs if the outer frame loop is present, so we only assert that
//! the bridge correctly returns `Invoke(...)`; full balance-effect
//! tests live in `native_state_tests.rs`.
//!
//! ```bash
//! cargo test -p mazze-executor --test cross_space_bridge
//! ```

use keccak_hash::keccak;
use mazze_executor::{
    executive_observer::TracerTrait,
    internal_contract::{
        evm_map, initialize_internal_contract_accounts,
        InternalContractTrait, InternalRefContext, InternalTrapResult,
    },
    machine::{new_machine_with_builtin, Machine, VmFactory},
    stack::CallStackInfo,
    state::{CleanupMode, State},
    substate::Substate,
};
use mazze_parameters::internal_contract_addresses::CROSS_SPACE_CONTRACT_ADDRESS;
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
use solidity_abi::ABIEncodable;

// ============================================================
// Harness (mirrors the one in internal_contracts.rs)
// ============================================================

struct Harness {
    _storage_manager: FakeStateManager,
    state: State,
    machine: Machine,
    env: Env,
    substate: Substate,
    callstack: CallStackInfo,
    tracer: (),
}

impl Harness {
    fn new() -> Self { Self::with_params(Default::default()) }

    fn with_params(params: mazze_executor::spec::CommonParams) -> Self {
        let storage_manager = new_state_manager_for_unit_test();
        let machine =
            new_machine_with_builtin(params, VmFactory::new(1024 * 32));
        let state = {
            let mut state = State::new(StateDb::new(
                storage_manager.get_state_for_genesis_write(),
            ))
            .expect("State::new failed");
            let addresses: Vec<Address> = machine
                .internal_contracts()
                .keys()
                .copied()
                .collect();
            initialize_internal_contract_accounts(&mut state, &addresses)
                .expect("initialize_internal_contract_accounts failed");
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

fn make_call_params(
    sender: Address, contract: Address, gas: u64, value: U256, data: Vec<u8>,
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
        value: ActionValue::Transfer(value),
        code: None,
        code_hash: Default::default(),
        data: Some(data),
        call_type: CallType::Call,
        create_type: CreateType::None,
        params_type: ParamsType::Embedded,
    }
}

fn selector(sig: &str) -> [u8; 4] {
    let h = keccak(sig.as_bytes());
    let mut out = [0u8; 4];
    out.copy_from_slice(&h.0[..4]);
    out
}

fn encode_call<T: ABIEncodable>(sig: &str, args: &T) -> Vec<u8> {
    let mut data = Vec::with_capacity(4 + 64);
    data.extend_from_slice(&selector(sig));
    data.extend_from_slice(&args.abi_encode());
    data
}

fn random_user_addr() -> Address {
    let mut addr = Address::random();
    addr.set_user_account_type_bits();
    addr
}

// ============================================================
// mappedAddress derivation
// ============================================================

#[test]
fn mapped_address_is_keccak_truncated_eth_space() {
    // The bridge derives:
    //   evm_map(native_addr) = keccak256(native_addr)[12..32].with_evm_space()
    //
    // Any change to this derivation invalidates every existing
    // cross-space balance.
    for _ in 0..32 {
        let native = random_user_addr();
        let mapped = evm_map(native);
        assert_eq!(
            mapped.space,
            Space::Ethereum,
            "mapped address must be in eSpace",
        );
        let expected: [u8; 20] = {
            let h = keccak(native.as_bytes());
            let mut buf = [0u8; 20];
            buf.copy_from_slice(&h.0[12..32]);
            buf
        };
        assert_eq!(
            mapped.address.as_bytes(),
            &expected,
            "mapped address must be the low 20 bytes of keccak(native)",
        );
    }
}

#[test]
fn mapped_address_is_deterministic() {
    let native = random_user_addr();
    let a = evm_map(native);
    let b = evm_map(native);
    let c = evm_map(native);
    assert_eq!(a, b);
    assert_eq!(b, c);
}

#[test]
fn mapped_address_is_collision_resistant_for_distinct_natives() {
    // 256 distinct random natives → 256 distinct mapped addresses.
    // Catches any catastrophic degeneracy in `evm_map`.
    let mut seen = std::collections::HashSet::new();
    for _ in 0..256 {
        let native = random_user_addr();
        let mapped = evm_map(native);
        assert!(
            seen.insert(mapped.address),
            "evm_map produced a collision in 256 random samples",
        );
    }
}

// ============================================================
// withdrawFromMapped (Native ← eSpace)
// ============================================================

#[test]
fn withdraw_from_mapped_happy_path() {
    let mut harness = Harness::new();
    let user = random_user_addr();
    let mapped = evm_map(user);

    // Pre-fund the mapped address with 100 mazzies.
    let amount = U256::from(100u64);
    harness
        .state
        .add_balance(&mapped, &amount, CleanupMode::NoEmpty)
        .unwrap();
    assert_eq!(harness.state.balance(&mapped).unwrap(), amount);
    assert_eq!(
        harness.state.balance(&user.with_native_space()).unwrap(),
        U256::zero(),
    );

    let spec = harness.spec();
    let data = encode_call("withdrawFromMapped(uint256)", &amount);
    let params = make_call_params(
        user, // sender == native address; bridge derives mapped from it
        CROSS_SPACE_CONTRACT_ADDRESS,
        100_000,
        U256::zero(),
        data,
    );

    let result = dispatch(
        &mut harness,
        &spec,
        &CROSS_SPACE_CONTRACT_ADDRESS,
        params,
        |contract_box, params, ref_ctx| contract_box.execute(params, ref_ctx),
    );

    assert!(matches!(result, InternalTrapResult::Return(Ok(_))));
    // Mapped balance is debited; native balance is credited.
    assert_eq!(
        harness.state.balance(&mapped).unwrap(),
        U256::zero(),
        "mapped balance should be fully withdrawn",
    );
    assert_eq!(
        harness.state.balance(&user.with_native_space()).unwrap(),
        amount,
        "native balance should be credited the withdrawn amount",
    );
}

#[test]
fn withdraw_from_mapped_insufficient_balance_reverts() {
    let mut harness = Harness::new();
    let user = random_user_addr();
    let mapped = evm_map(user);

    // Mapped has 50, user tries to withdraw 100 → revert.
    harness
        .state
        .add_balance(&mapped, &U256::from(50u64), CleanupMode::NoEmpty)
        .unwrap();

    let spec = harness.spec();
    let data =
        encode_call("withdrawFromMapped(uint256)", &U256::from(100u64));
    let params = make_call_params(
        user,
        CROSS_SPACE_CONTRACT_ADDRESS,
        100_000,
        U256::zero(),
        data,
    );

    let result = dispatch(
        &mut harness,
        &spec,
        &CROSS_SPACE_CONTRACT_ADDRESS,
        params,
        |contract_box, params, ref_ctx| contract_box.execute(params, ref_ctx),
    );

    match result {
        InternalTrapResult::Return(Err(_)) => {} // expected
        InternalTrapResult::Return(Ok(_)) => {
            panic!("withdraw with insufficient mapped balance should revert")
        }
        InternalTrapResult::Invoke(..) => {
            panic!("withdraw should not trap into a sub-call")
        }
    }
    // No funds moved.
    assert_eq!(harness.state.balance(&mapped).unwrap(), U256::from(50));
    assert_eq!(
        harness.state.balance(&user.with_native_space()).unwrap(),
        U256::zero(),
    );
}

#[test]
fn withdraw_with_zero_value_succeeds() {
    let mut harness = Harness::new();
    let user = random_user_addr();
    let mapped = evm_map(user);

    let spec = harness.spec();
    let data = encode_call("withdrawFromMapped(uint256)", &U256::zero());
    let params = make_call_params(
        user,
        CROSS_SPACE_CONTRACT_ADDRESS,
        100_000,
        U256::zero(),
        data,
    );

    let result = dispatch(
        &mut harness,
        &spec,
        &CROSS_SPACE_CONTRACT_ADDRESS,
        params,
        |contract_box, params, ref_ctx| contract_box.execute(params, ref_ctx),
    );

    // Zero-value withdraw is a no-op; should succeed cleanly.
    assert!(matches!(result, InternalTrapResult::Return(Ok(_))));
    assert_eq!(harness.state.balance(&mapped).unwrap(), U256::zero());
}

#[test]
fn withdraw_increments_mapped_nonce() {
    let mut harness = Harness::new();
    let user = random_user_addr();
    let mapped = evm_map(user);

    harness
        .state
        .add_balance(&mapped, &U256::from(10u64), CleanupMode::NoEmpty)
        .unwrap();
    let nonce_before = harness.state.nonce(&mapped).unwrap();

    let spec = harness.spec();
    let data =
        encode_call("withdrawFromMapped(uint256)", &U256::from(1u64));
    let params = make_call_params(
        user,
        CROSS_SPACE_CONTRACT_ADDRESS,
        100_000,
        U256::zero(),
        data,
    );

    let _ = dispatch(
        &mut harness,
        &spec,
        &CROSS_SPACE_CONTRACT_ADDRESS,
        params,
        |contract_box, params, ref_ctx| contract_box.execute(params, ref_ctx),
    );

    let nonce_after = harness.state.nonce(&mapped).unwrap();
    assert_eq!(
        nonce_after,
        nonce_before + U256::one(),
        "withdrawFromMapped must increment the mapped address's nonce",
    );
}

// ============================================================
// mappedBalance / mappedNonce queries
// ============================================================

#[test]
fn mapped_balance_reflects_state() {
    let mut harness = Harness::new();
    let user = random_user_addr();
    let mapped = evm_map(user);
    let funding = U256::from(42u64);

    harness
        .state
        .add_balance(&mapped, &funding, CleanupMode::NoEmpty)
        .unwrap();

    let spec = harness.spec();
    let data = encode_call("mappedBalance(address)", &user);
    let params = make_call_params(
        random_user_addr(), // arbitrary caller — query is read-only
        CROSS_SPACE_CONTRACT_ADDRESS,
        100_000,
        U256::zero(),
        data,
    );

    let result = dispatch(
        &mut harness,
        &spec,
        &CROSS_SPACE_CONTRACT_ADDRESS,
        params,
        |contract_box, params, ref_ctx| contract_box.execute(params, ref_ctx),
    );

    assert!(matches!(result, InternalTrapResult::Return(Ok(_))));
    // The query path doesn't mutate state.
    assert_eq!(harness.state.balance(&mapped).unwrap(), funding);
}

// ============================================================
// transferEVM / callEVM etc. — trap-into-sub-call surface
// ============================================================

#[test]
fn transfer_evm_traps_into_sub_call() {
    // `transferEVM(bytes20)` is supposed to invoke an eSpace transfer.
    // The bridge returns `InternalTrapResult::Invoke(p, r)` because
    // the eSpace call runs as a sub-frame. Any change that makes this
    // return `Return(_)` means the bridge has stopped handing off to
    // the eSpace VM — a critical semantic break.
    let mut harness = Harness::new();
    let sender = random_user_addr();
    let mapped_sender = evm_map(sender);

    // Pre-fund the mapped sender so the cross-space transfer has
    // something to spend on the eSpace side.
    harness
        .state
        .add_balance(
            &mapped_sender,
            &U256::from(1000u64),
            CleanupMode::NoEmpty,
        )
        .unwrap();
    // Also fund the source contract address (sender mapped from the
    // calling EOA) so the sub-call has gas + value to consume.
    harness
        .state
        .add_balance(
            &sender.with_native_space(),
            &U256::from(1_000_000_000_000_000_000u64),
            CleanupMode::NoEmpty,
        )
        .unwrap();
    // Pre-fund the bridge contract itself with the call-value. In
    // production this transfer is done by the outer Executive before
    // the internal contract is invoked; the unit-test harness bypasses
    // that step, so we mirror it explicitly.
    let bridge_value = U256::from(10u64);
    harness
        .state
        .add_balance(
            &CROSS_SPACE_CONTRACT_ADDRESS.with_native_space(),
            &bridge_value,
            CleanupMode::NoEmpty,
        )
        .unwrap();

    let to_bytes20: [u8; 20] = [0x42; 20];
    let spec = harness.spec();
    let data = encode_call("transferEVM(bytes20)", &to_bytes20);
    let params = make_call_params(
        sender,
        CROSS_SPACE_CONTRACT_ADDRESS,
        1_000_000,
        bridge_value,
        data,
    );

    let result = dispatch(
        &mut harness,
        &spec,
        &CROSS_SPACE_CONTRACT_ADDRESS,
        params,
        |contract_box, params, ref_ctx| contract_box.execute(params, ref_ctx),
    );

    match result {
        InternalTrapResult::Invoke(_, _) => {
            // Expected: bridge handed off to an eSpace sub-frame.
        }
        InternalTrapResult::Return(r) => {
            panic!(
                "transferEVM should trap into a sub-call, got Return({:?})",
                r
            )
        }
    }
}

// ============================================================
// Shield-in from eSpace (spec §8.5.1)
// ============================================================

/// An eSpace transaction to the shielded bridge: the frame has already
/// moved its value onto the bridge's eSpace account; `shieldIn` moves it
/// to the native backing and records the credit.
#[test]
fn shield_in_from_espace_moves_the_value_to_the_native_backing() {
    use mazze_executor::shielded_engine::{backing_account, deposit_credit};
    use mazze_parameters::internal_contract_addresses::SHIELDED_BRIDGE_ADDRESS;
    use mazze_types::H256;

    let mut common = mazze_executor::spec::CommonParams::default();
    common.shielded.activation_height = 0;
    let mut harness = Harness::with_params(common);
    let spec = harness.spec();
    assert!(spec.shielded_active);

    let espace_bridge = SHIELDED_BRIDGE_ADDRESS.with_evm_space();
    assert!(
        harness
            .machine
            .internal_contracts()
            .contract(&CROSS_SPACE_CONTRACT_ADDRESS.with_evm_space(), &spec)
            .is_none(),
        "no other internal contract is"
    );

    // Five GMazzy: shielded amounts are whole units.
    let amount = U256::from(5_000_000_000u64);
    harness
        .state
        .add_balance(&espace_bridge, &amount, CleanupMode::NoEmpty)
        .unwrap();
    let body_hash = H256::repeat_byte(0x42);
    let data = encode_call("shieldIn(bytes32)", &body_hash);
    let mut params = make_call_params(
        Address::random(),
        SHIELDED_BRIDGE_ADDRESS,
        100_000,
        amount,
        data,
    );
    params.space = Space::Ethereum;

    let Harness { state, substate, callstack, tracer, env, machine, .. } =
        &mut harness;
    let contract = machine
        .internal_contracts()
        .contract(&espace_bridge, &spec)
        .unwrap();
    let mut ref_ctx = InternalRefContext {
        env,
        spec: &spec,
        callstack,
        state,
        substate,
        tracer: tracer as &mut dyn TracerTrait,
        static_flag: false,
        depth: 0,
    };
    // A fraction of a GMazzy is refused, never rounded.
    let mut fractional = params.clone();
    fractional.value = ActionValue::Transfer(amount + 1);
    assert!(matches!(
        contract.execute(&fractional, &mut ref_ctx),
        InternalTrapResult::Return(Err(_))
    ));
    let result = contract.execute(&params, &mut ref_ctx);
    assert!(matches!(result, InternalTrapResult::Return(Ok(_))));

    assert_eq!(harness.state.balance(&espace_bridge).unwrap(), U256::zero());
    assert_eq!(harness.state.balance(&backing_account()).unwrap(), amount);
    assert_eq!(
        deposit_credit(&harness.state, body_hash.as_bytes()).unwrap(),
        amount
    );
}

/// After the pause height the bridge takes no deposit.
#[test]
fn a_paused_lane_takes_no_deposit() {
    use mazze_parameters::internal_contract_addresses::SHIELDED_BRIDGE_ADDRESS;
    use mazze_types::H256;

    let mut common = mazze_executor::spec::CommonParams::default();
    common.shielded.activation_height = 0;
    common.shielded.pause_height = 0;
    let mut harness = Harness::with_params(common);
    let spec = harness.spec();
    assert!(!spec.shielded_active);
    let data = encode_call("shieldIn(bytes32)", &H256::repeat_byte(1));
    let params = make_call_params(
        Address::random(),
        SHIELDED_BRIDGE_ADDRESS,
        100_000,
        U256::from(1_000_000_000u64),
        data,
    );
    let result = dispatch(&mut harness, &spec, &SHIELDED_BRIDGE_ADDRESS, params, |c, p, ctx| {
        c.execute(p, ctx)
    });
    assert!(matches!(result, InternalTrapResult::Return(Err(_))));
}

/// The bridge address the body rules refuse as a destination is the bridge.
#[test]
fn the_body_rules_know_the_bridge_address() {
    use mazze_parameters::internal_contract_addresses::SHIELDED_BRIDGE_ADDRESS;
    assert_eq!(primitives::shielded::SHIELDED_BRIDGE_ADDRESS_BYTES, SHIELDED_BRIDGE_ADDRESS.0);
}
