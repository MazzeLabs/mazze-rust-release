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
    spec::ShieldedParams,
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
        .add_balance(&backing_account(), &U256::from(b.v_public_in), CleanupMode::ForceCreate)
        .unwrap();
    record_deposit(state, b.hash().as_bytes(), U256::from(b.v_public_in), height).unwrap();
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
    assert_eq!(state.balance(&backing_account()).unwrap(), U256::from(10_000 - FEE), "fee burned");
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
    assert_eq!(backing, U256::from(10_000 - 3 * FEE - 3_000));
    assert_eq!(dest, U256::from(3_000));
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
            ShieldedOutcome::Skipped(SkipReason::UnknownAnchor),
            ShieldedOutcome::Skipped(SkipReason::UnknownAnchor),
            ShieldedOutcome::Skipped(SkipReason::UnknownAnchor),
        ],
        "root1 was recorded at height 3: outside a 50-epoch window at 54"
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
        U256::from(100)
    );
}
