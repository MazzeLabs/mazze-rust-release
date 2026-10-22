// Copyright 2026 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Stage B: applying shielded sections to the state.
//!
//! ```bash
//! cargo test -p mazze-executor --test shielded_engine
//! ```

use std::sync::Arc;

use mazze_executor::{
    shielded_engine::{
        backing_account, credit_key, execute_section, pool_account, record_deposit,
        ShieldedOutcome, SkipReason,
    },
    spec::{shielded_to_mazzy, ShieldedParams},
    state::{CleanupMode, State},
};
use mazze_shielded_pq::{
    field::{to_bytes, Xof},
    tree::Frontier,
};
use mazze_statedb::StateDb;
use mazze_storage::{
    state_manager::StateManagerTrait, tests::new_state_manager_for_unit_test,
};
use mazze_types::{Address, AddressSpaceUtil, Space, U256};
use primitives::shielded::{ShieldedDest, ShieldedDigest, ShieldedTxBody};

const FEE: u64 = 1_000;

/// A shielded amount (in units) in mazzy, as balances hold it.
fn m(units: u64) -> U256 { shielded_to_mazzy(units) }

fn digest(label: &str, i: u64) -> ShieldedDigest {
    let elements: [_; 12] = Xof::new(label, &[&i.to_le_bytes()]).fields();
    ShieldedDigest::from_bytes(&to_bytes(&elements)).unwrap()
}

fn body(seed: u32, anchor: ShieldedDigest, v_in: u64, v_out: u64) -> ShieldedTxBody {
    let mut b = ShieldedTxBody::sample_for_tests(seed);
    b.anchor = anchor;
    b.nf = [digest("nf", 2 * seed as u64), digest("nf", 2 * seed as u64 + 1)];
    b.cm_out = [digest("cm", 2 * seed as u64), digest("cm", 2 * seed as u64 + 1)];
    b.v_public_in = v_in;
    b.v_public_out = v_out;
    b.dest = (v_out > 0).then(|| ShieldedDest {
        space: Space::Native,
        address: Address::repeat_byte(0x55),
    });
    b.fee = FEE;
    b
}

fn root_after(cms: &[ShieldedDigest]) -> ShieldedDigest {
    let mut f = Frontier::default();
    for cm in cms {
        f.append(mazze_shielded_pq::field::from_bytes(cm.as_bytes()).unwrap());
    }
    ShieldedDigest::from_bytes(&to_bytes(&f.root())).unwrap()
}

fn empty_root() -> ShieldedDigest { root_after(&[]) }

fn params() -> ShieldedParams {
    ShieldedParams { activation_height: 0, slot_epochs: 1, anchor_window_epochs: 50, ..Default::default() }
}

/// What the bridge does for a shield-in: value into the backing, a credit
/// for the body.
fn deposit(state: &mut State, b: &ShieldedTxBody, height: u64) {
    state
        .add_balance(&backing_account(), &m(b.v_public_in), CleanupMode::ForceCreate)
        .unwrap();
    record_deposit(state, b.hash().as_bytes(), m(b.v_public_in), height).unwrap();
}

fn snapshot(state: &State) -> (U256, U256, U256) {
    (
        state.balance(&backing_account()).unwrap(),
        state.balance(&Address::repeat_byte(0x55).with_native_space()).unwrap(),
        state.storage_at(&pool_account(), b"leaves").unwrap(),
    )
}

#[test]
fn shield_in_pay_shield_out_and_every_refusal() {
    let storage_manager = new_state_manager_for_unit_test();
    let mut state =
        State::new(StateDb::new(storage_manager.get_state_for_genesis_write())).unwrap();
    let p = params();

    // The first section creates the pool and records the empty root. Its
    // transaction has an unknown anchor, so nothing is applied.
    let stray = Arc::new(body(9999, digest("x", 0), 0, 0));
    assert_eq!(
        execute_section(&mut state, &[stray], 1, &p).unwrap(),
        vec![ShieldedOutcome::Skipped(SkipReason::UnknownAnchor)]
    );

    // 1. Shield-in of 10_000, anchored at the empty tree.
    let shield_in = body(1, empty_root(), 10_000, 0);
    let unfunded = body(2, empty_root(), 10_000, 0);
    assert_eq!(
        execute_section(&mut state, &[Arc::new(unfunded)], 2, &p).unwrap(),
        vec![ShieldedOutcome::Skipped(SkipReason::NoDeposit)],
        "no deposit, no shield-in"
    );
    deposit(&mut state, &shield_in, 2);
    assert_eq!(
        execute_section(&mut state, &[Arc::new(shield_in.clone())], 3, &p).unwrap(),
        vec![ShieldedOutcome::Applied { positions: [0, 1] }]
    );
    assert_eq!(state.balance(&backing_account()).unwrap(), m(10_000 - FEE), "fee burned");
    assert!(
        state.storage_at(&pool_account(), &credit_key(shield_in.hash().as_bytes())).unwrap().is_zero(),
        "the credit is consumed"
    );

    // 2. A payment anchored at the new root, then a shield-out of 3_000.
    let root1 = root_after(&shield_in.cm_out);
    let pay = body(3, root1, 0, 0);
    let out = body(4, root_after(&[shield_in.cm_out[0], shield_in.cm_out[1], pay.cm_out[0], pay.cm_out[1]]), 0, 3_000);
    assert_eq!(
        execute_section(&mut state, &[Arc::new(pay.clone()), Arc::new(out.clone())], 4, &p).unwrap(),
        vec![
            ShieldedOutcome::Applied { positions: [2, 3] },
            ShieldedOutcome::Applied { positions: [4, 5] },
        ],
        "a section applies in order: the second spends at the first's root"
    );
    let (backing, dest, leaves) = snapshot(&state);
    assert_eq!(backing, m(10_000 - 3 * FEE - 3_000));
    assert_eq!(dest, m(3_000));
    assert_eq!(leaves, U256::from(6));

    // 3. Refusals leave the state exactly as it was.
    let before = snapshot(&state);
    let mut replay = body(5, root1, 0, 0);
    replay.nf[1] = pay.nf[0];
    let late = body(6, empty_root(), 0, 0);
    let greedy = body(7, root1, 0, 1_000_000);
    let outcomes = execute_section(
        &mut state,
        &[Arc::new(replay), Arc::new(late), Arc::new(greedy)],
        3 + 50 + 1,
        &p,
    )
    .unwrap();
    assert_eq!(
        outcomes,
        vec![
            ShieldedOutcome::Skipped(SkipReason::Spent),
            ShieldedOutcome::Skipped(SkipReason::UnknownAnchor),
            ShieldedOutcome::Skipped(SkipReason::Backing),
        ],
        "a replayed nullifier; the empty root, replaced at 2, is outside the window at 54; \
         a shield-out above the backing"
    );
    let mut replay = body(5, root1, 0, 0);
    replay.nf[1] = pay.nf[0];
    let greedy = body(7, root1, 0, 1_000_000);
    assert_eq!(
        execute_section(&mut state, &[Arc::new(replay), Arc::new(greedy)], 10, &p).unwrap(),
        vec![
            ShieldedOutcome::Skipped(SkipReason::Spent),
            ShieldedOutcome::Skipped(SkipReason::Backing),
        ]
    );
    assert_eq!(snapshot(&state), before, "skipped transactions change nothing");

    // 4. eSpace shield-out.
    let mut to_evm = body(8, root1, 0, 100);
    to_evm.dest = Some(ShieldedDest { space: Space::Ethereum, address: Address::repeat_byte(0x66) });
    assert!(matches!(
        execute_section(&mut state, &[Arc::new(to_evm)], 11, &p).unwrap()[0],
        ShieldedOutcome::Applied { .. }
    ));
    assert_eq!(
        state.balance(&Address::repeat_byte(0x66).with_evm_space()).unwrap(),
        m(100)
    );
}

fn fresh_state() -> (mazze_storage::tests::FakeStateManager, State) {
    let storage_manager = new_state_manager_for_unit_test();
    let state =
        State::new(StateDb::new(storage_manager.get_state_for_genesis_write())).unwrap();
    (storage_manager, state)
}

/// An idle lane does not lock itself: after a gap longer than the anchor
/// window, the current root is still a valid anchor.
#[test]
fn the_current_root_never_expires() {
    let (_sm, mut state) = fresh_state();
    let p = params();
    let shield_in = body(1, empty_root(), 10_000, 0);
    deposit(&mut state, &shield_in, 1);
    assert!(matches!(
        execute_section(&mut state, &[Arc::new(shield_in.clone())], 2, &p).unwrap()[0],
        ShieldedOutcome::Applied { .. }
    ));
    let current = root_after(&shield_in.cm_out);
    // Far past the window, nothing applied in between.
    let late = 2 + 10 * p.anchor_window_epochs;
    assert_eq!(
        execute_section(&mut state, &[Arc::new(body(2, current, 0, 0))], late, &p).unwrap(),
        vec![ShieldedOutcome::Applied { positions: [2, 3] }],
        "the current root is always valid"
    );
    // An older root is still bound by the window.
    let older = body(3, empty_root(), 0, 0);
    assert_eq!(
        execute_section(&mut state, &[Arc::new(older)], late + 1, &p).unwrap(),
        vec![ShieldedOutcome::Skipped(SkipReason::UnknownAnchor)]
    );
}

/// A top-up of someone else's credit does not block their shield-in, and
/// the turnstile holds throughout.
#[test]
fn top_ups_do_not_block_and_the_turnstile_holds() {
    use mazze_executor::shielded_engine::turnstile;
    let (_sm, mut state) = fresh_state();
    let p = params();
    let victim = body(1, empty_root(), 10_000, 0);
    deposit(&mut state, &victim, 1);
    // An attacker adds 1 to the victim's credit.
    let mut grief = victim.clone();
    grief.v_public_in = 1;
    state
        .add_balance(&backing_account(), &U256::one(), CleanupMode::ForceCreate)
        .unwrap();
    record_deposit(&mut state, victim.hash().as_bytes(), U256::one(), 1).unwrap();
    assert!(matches!(
        execute_section(&mut state, &[Arc::new(victim.clone())], 2, &p).unwrap()[0],
        ShieldedOutcome::Applied { .. }
    ));
    assert_eq!(
        state.storage_at(&pool_account(), &credit_key(victim.hash().as_bytes())).unwrap(),
        U256::one(),
        "the excess stays credited to the body"
    );
    let (total_in, total_out, total_burnt) = turnstile(&state).unwrap();
    assert_eq!(total_in, m(10_000));
    assert_eq!(total_out, U256::zero());
    assert_eq!(total_burnt, m(FEE));
    // backing = in - out - burnt + unconsumed credits (the 1)
    assert_eq!(
        state.balance(&backing_account()).unwrap(),
        m(10_000 - FEE) + U256::one()
    );
}

/// The circuit breaker: shield-outs may take 10% of the backing per window;
/// beyond it they are skipped (notes unspent) until the next window. The
/// floor keeps a small pool usable.
#[test]
fn the_circuit_breaker_bounds_shield_outs_per_window() {
    let (_sm, mut state) = fresh_state();
    let p = ShieldedParams {
        breaker_window_epochs: 100,
        breaker_fraction_bps: 1_000,
        breaker_floor: 0,
        anchor_window_epochs: 10_000,
        ..params()
    };
    let shield_in = body(1, empty_root(), 100_000, 0);
    deposit(&mut state, &shield_in, 1);
    execute_section(&mut state, &[Arc::new(shield_in.clone())], 2, &p).unwrap();
    let root = root_after(&shield_in.cm_out);
    // Backing 99,000 units: 9,900 may leave in this window.
    let out = |seed: u32, v: u64| Arc::new(body(seed, root, 0, v));
    let skipped = |o: &[ShieldedOutcome]| o == [ShieldedOutcome::Skipped(SkipReason::CircuitBreaker)];
    assert!(skipped(&execute_section(&mut state, &[out(2, 9_901)], 3, &p).unwrap()), "over 10%");
    assert!(matches!(
        execute_section(&mut state, &[out(3, 6_000)], 4, &p).unwrap()[0],
        ShieldedOutcome::Applied { .. }
    ));
    assert!(
        skipped(&execute_section(&mut state, &[out(4, 4_000)], 5, &p).unwrap()),
        "the window's total counts"
    );
    // The next window measures again.
    assert!(matches!(
        execute_section(&mut state, &[out(5, 4_000)], 100, &p).unwrap()[0],
        ShieldedOutcome::Applied { .. }
    ));
    // With a floor, a small amount always passes.
    let floored = ShieldedParams { breaker_floor: 50_000, ..p.clone() };
    assert!(matches!(
        execute_section(&mut state, &[out(6, 20_000)], 101, &floored).unwrap()[0],
        ShieldedOutcome::Applied { .. }
    ));
}

/// The lane's parameters come from the network, not from a node's config.
#[test]
fn the_network_fixes_the_shielded_parameters() {
    use mazze_executor::spec::TESTNET_CHAIN_ID;
    let testnet = ShieldedParams::for_network(TESTNET_CHAIN_ID);
    assert!(testnet.is_active(0) && testnet.is_slot(10));
    assert_eq!(testnet.slot_epochs, 10);
    assert_eq!(testnet.pause_height, u64::MAX);
    assert_eq!((testnet.breaker_window_epochs, testnet.breaker_fraction_bps), (125_000, 1_000));
    let other = ShieldedParams::for_network(TESTNET_CHAIN_ID + 1);
    assert!(!other.is_active(0) && !other.is_slot(10), "inactive elsewhere");
    let paused = ShieldedParams { pause_height: 20, ..testnet };
    assert!(paused.is_slot(10) && !paused.is_slot(20) && !paused.is_active(25));
}

/// An anchor's window runs from when its root was replaced: after a quiet
/// stretch longer than the window, a transaction proven against the root
/// that the next one replaces is still accepted (audit 2026-10-22: before,
/// it expired at once and stranded shield-in deposits).
#[test]
fn a_replaced_root_stays_valid_for_a_window() {
    let (_sm, mut state) = fresh_state();
    let p = params();
    let first = body(1, empty_root(), 10_000, 0);
    deposit(&mut state, &first, 1);
    execute_section(&mut state, &[Arc::new(first.clone())], 2, &p).unwrap();
    let root = root_after(&first.cm_out);
    // Quiet for far longer than the window; two wallets prove against
    // `root`. One lands and replaces it...
    let late = 2 + 10 * p.anchor_window_epochs;
    assert!(matches!(
        execute_section(&mut state, &[Arc::new(body(2, root, 0, 0))], late, &p).unwrap()[0],
        ShieldedOutcome::Applied { .. }
    ));
    // ...and the other, a little later, is still accepted.
    assert!(matches!(
        execute_section(&mut state, &[Arc::new(body(3, root, 0, 0))], late + p.anchor_window_epochs, &p)
            .unwrap()[0],
        ShieldedOutcome::Applied { .. }
    ));
    // But not beyond the window from when it was replaced.
    assert_eq!(
        execute_section(&mut state, &[Arc::new(body(4, root, 0, 0))], late + p.anchor_window_epochs + 1, &p)
            .unwrap(),
        vec![ShieldedOutcome::Skipped(SkipReason::UnknownAnchor)]
    );
}
