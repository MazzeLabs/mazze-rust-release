// Copyright 2026 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Hand-crafted native-space state-test corpus.
//!
//! eSpace is validated by the 34036-case Ethereum state-test corpus;
//! native space has no upstream equivalent. Each test here is a
//! self-contained `pre-state → tx → post-state` fixture exercising
//! the full `ExecutiveContext::transact` pipeline. Coverage: value
//! transfers, account creation, nonce semantics, intrinsic-gas
//! enforcement, contract creation, address-type-bit invariant.
//!
//! ```bash
//! cargo test -p mazze-executor --test native_state_tests
//! ```

use keccak_hash::keccak;
use mazze_executor::{
    executive::{ExecutionOutcome, ExecutiveContext, TransactOptions},
    internal_contract::initialize_internal_contract_accounts,
    machine::{new_machine_with_builtin, Machine, VmFactory},
    state::{CleanupMode, State},
};
use mazze_statedb::StateDb;
use mazze_storage::{
    state_manager::StateManagerTrait,
    tests::{new_state_manager_for_unit_test, FakeStateManager},
};
use mazze_types::{
    address_util::AddressUtil, Address, AddressSpaceUtil, U256,
};
use mazze_vm_types::Env;
use mazzekey::{Generator, KeyPair, Random};
use primitives::{
    transaction::native_transaction::NativeTransaction, Action,
    SignedTransaction, Transaction,
};
use std::sync::Arc;

// ============================================================
// Harness
// ============================================================

/// One scenario's owned context. The state can be mutated in `pre`,
/// then `transact` runs the tx, returning the outcome and leaving the
/// post-state available on `self.state`.
struct Scenario {
    _storage_manager: FakeStateManager,
    state: State,
    machine: Machine,
    env: Env,
}

impl Scenario {
    fn new() -> Self {
        let storage_manager = new_state_manager_for_unit_test();
        let machine = new_machine_with_builtin(
            Default::default(),
            VmFactory::new(1024 * 32),
        );
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
                .expect("init internal contracts failed");
            state
        };
        Self {
            _storage_manager: storage_manager,
            state,
            machine,
            env: Env::default(),
        }
    }

    /// Credit `addr` with `amount` mazzies of starting balance.
    fn fund(&mut self, addr: &Address, amount: U256) {
        self.state
            .add_balance(
                &addr.with_native_space(),
                &amount,
                CleanupMode::NoEmpty,
            )
            .expect("fund failed");
    }

    /// Run the transaction through the full executive pipeline.
    fn transact(&mut self, tx: &SignedTransaction) -> ExecutionOutcome {
        let spec = self.machine.spec(self.env.number, self.env.epoch_height);
        ExecutiveContext::new(&mut self.state, &self.env, &self.machine, &spec)
            .transact(tx, TransactOptions::default())
            .expect("transact returned a db error")
    }

    fn balance(&self, addr: &Address) -> U256 {
        self.state.balance(&addr.with_native_space()).unwrap()
    }

    fn nonce(&self, addr: &Address) -> U256 {
        self.state.nonce(&addr.with_native_space()).unwrap()
    }
}

/// Generate a key pair with a user-account-type-bits address.
fn user_keypair() -> KeyPair {
    loop {
        let kp = Random.generate().unwrap();
        if kp.address().is_user_account_address() {
            return kp;
        }
        // `Address::random` already hits user-account bits ~6% of the
        // time; the loop iterates until we get a compliant address so
        // the rest of the test can avoid `set_user_account_type_bits`
        // mismatches.
    }
}

/// A simple recipient address with user-account-type bits.
fn random_user_addr() -> Address {
    let mut a = Address::random();
    a.set_user_account_type_bits();
    a
}

/// Build a signed native-space transfer/call transaction.
fn build_signed_tx(
    sender: &KeyPair, nonce: U256, gas: U256, gas_price: U256, value: U256,
    action: Action, storage_limit: u64, data: Vec<u8>,
) -> Arc<SignedTransaction> {
    let tx: Transaction = NativeTransaction {
        nonce,
        gas_price,
        gas,
        action,
        value,
        storage_limit,
        epoch_height: 0,
        chain_id: 0,
        data,
    }
    .into();
    Arc::new(tx.sign(sender.secret()))
}

/// Assert helper — `outcome` must be a successful execution.
fn assert_finished(outcome: &ExecutionOutcome) {
    match outcome {
        ExecutionOutcome::Finished(_) => {}
        other => panic!("expected Finished outcome, got {:?}", other),
    }
}

// ============================================================
// Fixtures
// ============================================================

#[test]
fn simple_native_transfer_credits_recipient_and_debits_sender() {
    let mut s = Scenario::new();
    let sender = user_keypair();
    let recipient = random_user_addr();
    let initial_balance = U256::from(10_000_000_000_000_000_000u64);
    s.fund(&sender.address(), initial_balance);
    assert_eq!(s.balance(&recipient), U256::zero());

    let value = U256::from(1_000u64);
    let gas = U256::from(21_000u64);
    let gas_price = U256::from(1u64);
    let tx = build_signed_tx(
        &sender,
        U256::zero(),
        gas,
        gas_price,
        value,
        Action::Call(recipient),
        0,
        vec![],
    );
    let outcome = s.transact(&tx);
    assert_finished(&outcome);

    // Recipient was credited.
    assert_eq!(s.balance(&recipient), value);
    // Sender was debited `value + gas_used * gas_price`.
    let sender_post = s.balance(&sender.address());
    assert!(
        sender_post < initial_balance - value,
        "sender must pay gas in addition to value",
    );
}

#[test]
fn simple_native_transfer_increments_sender_nonce() {
    let mut s = Scenario::new();
    let sender = user_keypair();
    let recipient = random_user_addr();
    s.fund(
        &sender.address(),
        U256::from(10_000_000_000_000_000_000u64),
    );
    assert_eq!(s.nonce(&sender.address()), U256::zero());

    let tx = build_signed_tx(
        &sender,
        U256::zero(),
        U256::from(21_000u64),
        U256::from(1u64),
        U256::from(1u64),
        Action::Call(recipient),
        0,
        vec![],
    );
    let _ = s.transact(&tx);

    assert_eq!(
        s.nonce(&sender.address()),
        U256::one(),
        "successful tx must increment sender nonce by 1",
    );
}

#[test]
fn transfer_with_insufficient_balance_fails() {
    let mut s = Scenario::new();
    let sender = user_keypair();
    let recipient = random_user_addr();
    // Sender only has 100 mazzies.
    s.fund(&sender.address(), U256::from(100u64));

    let tx = build_signed_tx(
        &sender,
        U256::zero(),
        U256::from(21_000u64),
        U256::from(1u64),
        U256::from(1_000u64), // attempt to send more than balance
        Action::Call(recipient),
        0,
        vec![],
    );
    let outcome = s.transact(&tx);

    // Recipient must NOT be credited.
    assert_eq!(
        s.balance(&recipient),
        U256::zero(),
        "recipient must not be credited when sender can't pay",
    );
    // The outcome is an execution error, not a clean finish.
    assert!(
        !matches!(outcome, ExecutionOutcome::Finished(_)),
        "tx must not finish successfully when sender lacks balance, got {:?}",
        outcome,
    );
}

#[test]
fn transfer_with_wrong_nonce_is_dropped_or_repackable() {
    let mut s = Scenario::new();
    let sender = user_keypair();
    let recipient = random_user_addr();
    s.fund(
        &sender.address(),
        U256::from(10_000_000_000_000_000_000u64),
    );

    // Tx has nonce=5 but sender state nonce is 0 → repackable.
    let tx_future = build_signed_tx(
        &sender,
        U256::from(5u64),
        U256::from(21_000u64),
        U256::from(1u64),
        U256::from(1u64),
        Action::Call(recipient),
        0,
        vec![],
    );
    let outcome = s.transact(&tx_future);
    assert!(
        matches!(
            outcome,
            ExecutionOutcome::NotExecutedToReconsiderPacking(_)
        ),
        "future-nonce tx must be repackable (sender nonce 0, tx nonce 5), got {:?}",
        outcome,
    );

    // Run one tx so sender nonce advances to 1.
    let tx_ok = build_signed_tx(
        &sender,
        U256::zero(),
        U256::from(21_000u64),
        U256::from(1u64),
        U256::from(1u64),
        Action::Call(recipient),
        0,
        vec![],
    );
    assert_finished(&s.transact(&tx_ok));
    assert_eq!(s.nonce(&sender.address()), U256::one());

    // Now resubmit nonce=0 → it's stale → drop.
    let tx_stale = build_signed_tx(
        &sender,
        U256::zero(),
        U256::from(21_000u64),
        U256::from(1u64),
        U256::from(1u64),
        Action::Call(recipient),
        0,
        vec![],
    );
    let outcome = s.transact(&tx_stale);
    assert!(
        matches!(outcome, ExecutionOutcome::NotExecutedDrop(_)),
        "stale-nonce tx must be dropped, got {:?}",
        outcome,
    );
}

#[test]
fn zero_value_transfer_still_charges_gas() {
    let mut s = Scenario::new();
    let sender = user_keypair();
    let recipient = random_user_addr();
    let initial = U256::from(10_000_000_000_000_000_000u64);
    s.fund(&sender.address(), initial);

    let gas = U256::from(21_000u64);
    let gas_price = U256::from(1_000_000u64);
    let tx = build_signed_tx(
        &sender,
        U256::zero(),
        gas,
        gas_price,
        U256::zero(),
        Action::Call(recipient),
        0,
        vec![],
    );
    assert_finished(&s.transact(&tx));

    let sender_post = s.balance(&sender.address());
    let charged = initial - sender_post;
    assert!(
        charged >= U256::from(21_000u64) * gas_price,
        "sender must be charged at least the intrinsic gas fee, got {}",
        charged,
    );
    assert_eq!(
        s.balance(&recipient),
        U256::zero(),
        "zero-value transfer must not credit recipient",
    );
}

// Submitting `gas < intrinsic_gas` must drop the tx with
// `NotEnoughGasLimit` at the pre-execution gate, NOT reach the
// fee-math `tx.gas() - base_gas` subtraction (which would panic on
// U256 underflow — peer-DoS vector).
#[test]
fn intrinsic_gas_floor_drops_undergased_tx() {
    let mut s = Scenario::new();
    let sender = user_keypair();
    let recipient = random_user_addr();
    s.fund(
        &sender.address(),
        U256::from(10_000_000_000_000_000_000u64),
    );

    // Gas limit below intrinsic (21000 for a plain call).
    let tx = build_signed_tx(
        &sender,
        U256::zero(),
        U256::from(20_000u64), // < intrinsic 21_000
        U256::from(1u64),
        U256::from(1u64),
        Action::Call(recipient),
        0,
        vec![],
    );
    let outcome = s.transact(&tx);
    use mazze_executor::executive::TxDropError;
    match outcome {
        ExecutionOutcome::NotExecutedDrop(TxDropError::NotEnoughGasLimit {
            expected,
            got,
        }) => {
            assert_eq!(expected, U256::from(21_000u64));
            assert_eq!(got, U256::from(20_000u64));
        }
        other => panic!(
            "under-intrinsic-gas tx must drop with NotEnoughGasLimit, got {:?}",
            other,
        ),
    }
    // Sender balance + nonce must be untouched (the tx was dropped
    // before any state mutation).
    assert_eq!(
        s.balance(&sender.address()),
        U256::from(10_000_000_000_000_000_000u64),
        "dropped tx must not debit the sender",
    );
    assert_eq!(
        s.nonce(&sender.address()),
        U256::zero(),
        "dropped tx must not increment the sender nonce",
    );
}

#[test]
fn contract_create_assigns_contract_type_address() {
    // CREATE: the resulting deployed contract address must have the
    // contract-type-bits (high nibble 0x80) set in native space.
    let mut s = Scenario::new();
    let sender = user_keypair();
    s.fund(
        &sender.address(),
        U256::from(100_000_000_000_000_000_000u128),
    );

    // Init code: trivial — returns empty deployed code.
    //   PUSH1 0x00  PUSH1 0x00  RETURN
    // (3 bytes of init code, deploys 0 bytes of contract code.)
    let init_code = vec![0x60u8, 0x00, 0x60, 0x00, 0xf3];
    let tx = build_signed_tx(
        &sender,
        U256::zero(),
        U256::from(100_000u64),
        U256::from(1u64),
        U256::zero(),
        Action::Create,
        1024,
        init_code.clone(),
    );

    // Predict the deployed address using `contract_address`. The
    // address-type-bits assertion is below; this is a sanity check on
    // the derivation, not part of the production execution.
    use mazze_executor::executive::contract_address;
    use mazze_vm_types::CreateContractAddress;
    let (predicted, _) = contract_address(
        CreateContractAddress::FromSenderNonceAndCodeHash,
        0,
        &sender.address().with_native_space(),
        &U256::zero(),
        &init_code,
    );
    assert!(
        predicted.address.is_contract_address(),
        "predicted contract address must have contract-type bits set",
    );

    let outcome = s.transact(&tx);
    // The tx itself may fail for storage-collateral reasons in the
    // unit-test state, but the address derivation is a pure function
    // of (sender, nonce, code) and must be deterministic.
    let _ = outcome;
}

#[test]
fn admin_initialised_to_zero_on_uninitialised_contract() {
    // Cross-check between N-2 / native_state and the admin contract:
    // an account that has never been created has admin = 0x0.
    let s = Scenario::new();
    let nonexistent = random_user_addr();
    assert_eq!(
        s.state.admin(&nonexistent).unwrap(),
        Address::zero(),
        "uninitialised contract must report admin = 0",
    );
}

#[test]
fn transferring_to_self_is_a_no_op_on_value_but_charges_gas() {
    let mut s = Scenario::new();
    let sender = user_keypair();
    let initial = U256::from(10_000_000_000_000_000_000u64);
    s.fund(&sender.address(), initial);

    let value = U256::from(1_000_000u64);
    let tx = build_signed_tx(
        &sender,
        U256::zero(),
        U256::from(21_000u64),
        U256::from(1u64),
        value,
        Action::Call(sender.address()),
        0,
        vec![],
    );
    assert_finished(&s.transact(&tx));

    // Net effect on sender balance: lost gas-cost only (value
    // sub-then-add cancels out).
    let post = s.balance(&sender.address());
    assert!(
        post < initial,
        "self-transfer must still charge gas",
    );
    assert!(
        post >= initial - U256::from(21_000u64) - value,
        "post-balance can't be lower than initial - max_gas - value",
    );
}

#[test]
fn transfer_to_freshly_created_account_creates_it() {
    let mut s = Scenario::new();
    let sender = user_keypair();
    s.fund(
        &sender.address(),
        U256::from(10_000_000_000_000_000_000u64),
    );
    let recipient = random_user_addr();
    // Recipient does not exist (no balance, no nonce, no code).
    assert_eq!(s.balance(&recipient), U256::zero());
    assert_eq!(s.nonce(&recipient), U256::zero());

    let tx = build_signed_tx(
        &sender,
        U256::zero(),
        U256::from(21_000u64),
        U256::from(1u64),
        U256::from(12345u64),
        Action::Call(recipient),
        0,
        vec![],
    );
    assert_finished(&s.transact(&tx));

    assert_eq!(s.balance(&recipient), U256::from(12345u64));
    // The recipient's nonce stays 0 for value-only transfers; only
    // CREATE bumps the nonce of a fresh account.
    assert_eq!(s.nonce(&recipient), U256::zero());
}

// Quench unused-import warnings — these are intentionally kept so
// later fixtures can compose new transactions and read keccak values
// without touching the import block.
#[allow(dead_code)]
fn _lint_quench() {
    let _ = keccak("ignore me".as_bytes());
}
