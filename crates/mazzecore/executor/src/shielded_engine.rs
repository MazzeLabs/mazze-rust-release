// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Stage B of the shielded pipeline (docs/internal/privacy-pq-stark.md §8.2):
//! applying a shielded section to the state. The proofs were verified when
//! the block was accepted (stage A); this stage needs only the state and
//! never the proof.
//!
//! State lives in two internal accounts at `SHIELDED_BRIDGE_ADDRESS`:
//! - in `Space::Shielded`, the pool's consensus state: spent nullifiers, the
//!   commitment tree frontier, the recorded roots (anchors), and deposit
//!   credits for shield-in;
//! - in `Space::Native`, the backing balance: all value inside the shielded
//!   space, which pays shield-outs and the burned fees.
//!
//! A transaction that fails a rule is skipped: every rule is checked before
//! anything is written, so a transaction applies entirely or not at all.

use std::sync::Arc;

use mazze_parameters::internal_contract_addresses::SHIELDED_BRIDGE_ADDRESS;
use mazze_shielded_pq::{
    field::{from_bytes, to_bytes, Digest, DIGEST},
    tree::{Frontier, DEPTH},
};
use mazze_statedb::Result as DbResult;
use mazze_types::{
    AddressSpaceUtil, AddressWithSpace, Space, U256,
};
use primitives::{
    shielded::{ShieldedDest, ShieldedDigest, ShieldedTxBody},
    storage::STORAGE_LAYOUT_REGULAR_V0,
};

use crate::{
    spec::{shielded_to_mazzy, ShieldedParams},
    state::{CleanupMode, State},
    substate::Substate,
};

/// The pool's consensus state account.
pub fn pool_account() -> AddressWithSpace {
    SHIELDED_BRIDGE_ADDRESS.with_shielded_space()
}

/// The account whose native balance backs every shielded note.
pub fn backing_account() -> AddressWithSpace {
    SHIELDED_BRIDGE_ADDRESS.with_native_space()
}

/// What happened to one shielded transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShieldedOutcome {
    /// Applied; its two output commitments are at these tree positions.
    Applied { positions: [u64; 2] },
    /// Skipped without any state change.
    Skipped(SkipReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// A shield-in without a matching deposit credit.
    NoDeposit,
    /// The anchor is not a recorded root, or is outside the window.
    UnknownAnchor,
    /// A nullifier was already spent.
    Spent,
    /// The backing balance does not cover the shield-out and fee. Never
    /// expected: it would mean the turnstile invariant is broken.
    Backing,
    /// The commitment tree is full.
    TreeFull,
    /// The shield-out exceeds what the circuit breaker allows in this
    /// window (spec §8.4). The notes stay unspent; it can be sent again in
    /// a later window.
    CircuitBreaker,
}

impl SkipReason {
    fn code(self) -> u8 {
        match self {
            SkipReason::NoDeposit => 1,
            SkipReason::UnknownAnchor => 2,
            SkipReason::Spent => 3,
            SkipReason::Backing => 4,
            SkipReason::TreeFull => 5,
            SkipReason::CircuitBreaker => 6,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => SkipReason::NoDeposit,
            2 => SkipReason::UnknownAnchor,
            3 => SkipReason::Spent,
            4 => SkipReason::Backing,
            5 => SkipReason::TreeFull,
            6 => SkipReason::CircuitBreaker,
            _ => return None,
        })
    }
}

/// Outcomes are kept by the node (not in consensus state) so wallets can
/// learn which outputs entered the tree and where: `[0, p0, p1]` when
/// applied, `[1, reason]` when skipped.
impl rlp::Encodable for ShieldedOutcome {
    fn rlp_append(&self, s: &mut rlp::RlpStream) {
        match self {
            ShieldedOutcome::Applied { positions } => {
                s.begin_list(3).append(&0u8).append(&positions[0]).append(&positions[1]);
            }
            ShieldedOutcome::Skipped(reason) => {
                s.begin_list(2).append(&1u8).append(&reason.code());
            }
        }
    }
}

impl rlp::Decodable for ShieldedOutcome {
    fn decode(rlp: &rlp::Rlp) -> Result<Self, rlp::DecoderError> {
        match (rlp.val_at::<u8>(0)?, rlp.item_count()?) {
            (0, 3) => Ok(ShieldedOutcome::Applied {
                positions: [rlp.val_at(1)?, rlp.val_at(2)?],
            }),
            (1, 2) => SkipReason::from_code(rlp.val_at(1)?)
                .map(ShieldedOutcome::Skipped)
                .ok_or(rlp::DecoderError::Custom("Unknown skip reason")),
            _ => Err(rlp::DecoderError::Custom("Malformed shielded outcome")),
        }
    }
}

// ---- storage keys -------------------------------------------------------

fn nf_key(nf: &ShieldedDigest) -> Vec<u8> { [b"nf".as_ref(), nf.as_bytes()].concat() }

fn root_key(root: &[u8]) -> Vec<u8> { [b"root".as_ref(), root].concat() }

/// Key of the deposit credit for a shield-in body, written by the bridge.
pub fn credit_key(body_hash: &[u8]) -> Vec<u8> {
    [b"credit".as_ref(), body_hash].concat()
}

const LEAVES_KEY: &[u8] = b"leaves";

fn frontier_key(level: usize, half: u8) -> Vec<u8> {
    vec![b'f', b'r', level as u8, half]
}

// ---- storage helpers ----------------------------------------------------

fn get(state: &State, key: &[u8]) -> DbResult<U256> {
    state.storage_at(&pool_account(), key)
}

fn set(state: &mut State, key: Vec<u8>, value: U256) -> DbResult<()> {
    state.set_storage(
        &pool_account(),
        key,
        value,
        Default::default(),
        &mut Substate::new(),
    )
}

/// A 48-byte digest as two storage words of 24 bytes each.
fn digest_words(bytes: &[u8]) -> [U256; 2] {
    [
        U256::from_big_endian(&bytes[..24]),
        U256::from_big_endian(&bytes[24..48]),
    ]
}

fn words_digest(words: [U256; 2]) -> Digest {
    let mut bytes = [0u8; 48];
    let mut word = [0u8; 32];
    words[0].to_big_endian(&mut word);
    bytes[..24].copy_from_slice(&word[8..]);
    words[1].to_big_endian(&mut word);
    bytes[24..].copy_from_slice(&word[8..]);
    from_bytes::<DIGEST>(&bytes).expect("the state holds canonical digests")
}

fn load_frontier(state: &State) -> DbResult<Frontier> {
    let leaves = get(state, LEAVES_KEY)?.as_u64();
    let mut frontier = Frontier { leaves, ..Default::default() };
    for level in 0..DEPTH {
        if (leaves >> level) & 1 == 1 {
            frontier.filled[level] = words_digest([
                get(state, &frontier_key(level, 0))?,
                get(state, &frontier_key(level, 1))?,
            ]);
        }
    }
    Ok(frontier)
}

fn store_frontier(state: &mut State, frontier: &Frontier) -> DbResult<()> {
    set(state, LEAVES_KEY.to_vec(), frontier.leaves.into())?;
    for level in 0..DEPTH {
        if (frontier.leaves >> level) & 1 == 1 {
            let [hi, lo] = digest_words(&to_bytes(&frontier.filled[level]));
            set(state, frontier_key(level, 0), hi)?;
            set(state, frontier_key(level, 1), lo)?;
        }
    }
    Ok(())
}

/// Records `root` as an anchor seen at `height` (stored as `height + 1`, so
/// that zero means unknown).
fn record_root(state: &mut State, root: &Digest, height: u64) -> DbResult<()> {
    set(state, root_key(&to_bytes(root)), U256::from(height) + 1)
}

/// Creates the pool account on first use and records the empty tree's root,
/// so the first transactions have an anchor.
fn ensure_initialized(state: &mut State, height: u64) -> DbResult<()> {
    if state.exists(&pool_account())? {
        return Ok(());
    }
    state.new_contract_with_admin(
        &pool_account(),
        &Default::default(),
        U256::zero(),
        Some(STORAGE_LAYOUT_REGULAR_V0),
    )?;
    record_root(state, &Frontier::default().root(), height)
}

fn digest_of(d: &ShieldedDigest) -> Digest {
    from_bytes::<DIGEST>(d.as_bytes()).expect("a decoded body holds canonical digests")
}

/// The height at which `root` was recorded: after the transaction that
/// produced it (or, for the empty tree, at initialization). Roots are never
/// deleted, which lets history from a peer be checked against the state.
pub fn root_height(state: &State, root: &Digest) -> DbResult<Option<u64>> {
    let seen = get(state, &root_key(&to_bytes(root)))?;
    Ok((!seen.is_zero()).then(|| seen.as_u64() - 1))
}

/// Whether `nf` is spent in `state`.
pub fn nullifier_spent(state: &State, nf: &ShieldedDigest) -> DbResult<bool> {
    Ok(!get(state, &nf_key(nf))?.is_zero())
}

/// Whether `anchor` is acceptable at `height`: the current tree root
/// always is (an idle lane must not lock itself); an older root while the
/// window since it was replaced is open.
pub fn anchor_valid(
    state: &State, anchor: &ShieldedDigest, height: u64, params: &ShieldedParams,
) -> DbResult<bool> {
    let (current, _) = tree_state(state)?;
    if anchor.as_bytes()[..] == to_bytes(&current)[..] {
        return Ok(true);
    }
    let seen = get(state, &root_key(anchor.as_bytes()))?;
    Ok(!seen.is_zero()
        && height.saturating_sub(seen.as_u64() - 1) <= params.anchor_window_epochs)
}

/// Whether a shield-in body's deposit is credited (at least its
/// `v_public_in`). Bodies without a shield-in need none.
pub fn deposit_covers(state: &State, body: &ShieldedTxBody) -> DbResult<bool> {
    Ok(body.v_public_in == 0
        || get(state, &credit_key(body.hash().as_bytes()))?
            >= shielded_to_mazzy(body.v_public_in))
}

/// Turnstile counters: value that entered and left the shielded space and
/// fees burned in it. `backing == in - out - burnt` must always hold.
const TOTAL_IN_KEY: &[u8] = b"total_in";
const TOTAL_OUT_KEY: &[u8] = b"total_out";
const TOTAL_BURNT_KEY: &[u8] = b"total_burnt";

/// The circuit breaker's window: its index + 1, the backing it is measured
/// against, and what shield-outs took from it so far (mazzy).
const CB_WINDOW_KEY: &[u8] = b"cb_window";
const CB_BASE_KEY: &[u8] = b"cb_base";
const CB_OUT_KEY: &[u8] = b"cb_out";

/// What the circuit breaker still lets shield-outs take at `height`, in
/// mazzy (spec §8.4): `breaker_fraction_bps` of the larger of the backing
/// when the window opened and the backing now, at least `breaker_floor`,
/// minus what this window already paid out.
pub fn breaker_allowance(
    state: &State, height: u64, params: &ShieldedParams,
) -> DbResult<U256> {
    let window = U256::from(height / params.breaker_window_epochs.max(1) + 1);
    let backing = state.balance(&backing_account())?;
    let (base, out) = if get(state, CB_WINDOW_KEY)? == window {
        (get(state, CB_BASE_KEY)?, get(state, CB_OUT_KEY)?)
    } else {
        (backing, U256::zero())
    };
    let limit = (base.max(backing) * U256::from(params.breaker_fraction_bps)
        / U256::from(10_000))
    .max(shielded_to_mazzy(params.breaker_floor));
    Ok(limit.saturating_sub(out))
}

fn breaker_record(
    state: &mut State, height: u64, params: &ShieldedParams, amount: U256,
) -> DbResult<()> {
    let window = U256::from(height / params.breaker_window_epochs.max(1) + 1);
    if get(state, CB_WINDOW_KEY)? != window {
        let backing = state.balance(&backing_account())?;
        set(state, CB_WINDOW_KEY.to_vec(), window)?;
        set(state, CB_BASE_KEY.to_vec(), backing)?;
        set(state, CB_OUT_KEY.to_vec(), U256::zero())?;
    }
    let out = get(state, CB_OUT_KEY)? + amount;
    set(state, CB_OUT_KEY.to_vec(), out)
}

/// Deposits credited by the bridge and not yet consumed by a shield-in.
const PENDING_CREDITS_KEY: &[u8] = b"pending_credits";

/// Deposits credited and not yet consumed by a shield-in (mazzy).
pub fn pending_credits(state: &State) -> DbResult<U256> { get(state, PENDING_CREDITS_KEY) }

pub fn turnstile(state: &State) -> DbResult<(U256, U256, U256)> {
    Ok((
        get(state, TOTAL_IN_KEY)?,
        get(state, TOTAL_OUT_KEY)?,
        get(state, TOTAL_BURNT_KEY)?,
    ))
}

/// The current root of the commitment tree and its number of leaves.
pub fn tree_state(state: &State) -> DbResult<(Digest, u64)> {
    if !state.exists(&pool_account())? {
        let empty = Frontier::default();
        return Ok((empty.root(), 0));
    }
    let frontier = load_frontier(state)?;
    Ok((frontier.root(), frontier.leaves))
}

/// The credit currently recorded for a shield-in body.
pub fn deposit_credit(state: &State, body_hash: &[u8]) -> DbResult<U256> {
    get(state, &credit_key(body_hash))
}

/// The bridge's half of a shield-in: the depositor's `amount` has already
/// moved into the backing account; this records the credit the shielded
/// body `body_hash` consumes when its slot executes. A second deposit for
/// the same body adds to the credit; the shield-in still consumes exactly
/// its `v_public_in` and the excess stays credited to the body.
pub fn record_deposit(
    state: &mut State, body_hash: &[u8], amount: U256, height: u64,
) -> DbResult<()> {
    ensure_initialized(state, height)?;
    let key = credit_key(body_hash);
    let credit = get(state, &key)?.saturating_add(amount);
    set(state, key, credit)?;
    let pending = pending_credits(state)?.saturating_add(amount);
    set(state, PENDING_CREDITS_KEY.to_vec(), pending)
}

// ---- the stage --------------------------------------------------------------

/// Applies the shielded section of the epoch's pivot block, in order.
pub fn execute_section(
    state: &mut State, section: &[Arc<ShieldedTxBody>], height: u64,
    params: &ShieldedParams,
) -> DbResult<Vec<ShieldedOutcome>> {
    if section.is_empty() {
        return Ok(Vec::new());
    }
    ensure_initialized(state, height)?;
    let mut outcomes = Vec::with_capacity(section.len());
    for body in section {
        outcomes.push(apply(state, body, height, params)?);
    }
    // The turnstile (spec §8.4): every unit in the backing entered through
    // a shield-in and has not left. Credits not yet consumed sit in the
    // backing too.
    let (total_in, total_out, total_burnt) = turnstile(state)?;
    let backing = state.balance(&backing_account())?;
    let expected = (total_in + pending_credits(state)?)
        .saturating_sub(total_out + total_burnt);
    // Only a deficit is a breach (value left that never entered). A
    // surplus is someone's donation: a SELFDESTRUCT naming the backing as
    // beneficiary can add to it, but nothing can take from it.
    if backing < expected {
        error!(
            "SHIELDED TURNSTILE BREACH at height {}: backing {} below {}",
            height, backing, expected
        );
    } else if backing > expected {
        debug!(
            "shielded backing {} above the turnstile's {} by donations",
            backing, expected
        );
    }
    Ok(outcomes)
}

fn apply(
    state: &mut State, body: &ShieldedTxBody, height: u64,
    params: &ShieldedParams,
) -> DbResult<ShieldedOutcome> {
    use ShieldedOutcome::Skipped;

    // 1. Shield-in: the bridge holds at least this deposit for this body.
    //    A larger credit (a top-up by anyone) does not block it; the excess
    //    stays credited to the body.
    let credit = credit_key(body.hash().as_bytes());
    let credited = get(state, &credit)?;
    if body.v_public_in > 0 && credited < shielded_to_mazzy(body.v_public_in) {
        return Ok(Skipped(SkipReason::NoDeposit));
    }

    // 2. The anchor: the current root, or a recorded root in the window.
    if !anchor_valid(state, &body.anchor, height, params)? {
        return Ok(Skipped(SkipReason::UnknownAnchor));
    }

    // 3. Both nullifiers unspent (stage A already made them distinct).
    for nf in &body.nf {
        if !get(state, &nf_key(nf))?.is_zero() {
            return Ok(Skipped(SkipReason::Spent));
        }
    }

    // 4. The backing pays the shield-out and the burned fee. The deposit
    //    of a shield-in is already in it.
    let outflow = shielded_to_mazzy(body.v_public_out) + shielded_to_mazzy(body.fee);
    if state.balance(&backing_account())? < outflow {
        return Ok(Skipped(SkipReason::Backing));
    }

    // 5. The circuit breaker bounds what can leave per window.
    let v_out = shielded_to_mazzy(body.v_public_out);
    if body.v_public_out > 0 && v_out > breaker_allowance(state, height, params)? {
        return Ok(Skipped(SkipReason::CircuitBreaker));
    }

    let mut frontier = load_frontier(state)?;
    if frontier.leaves + 2 > mazze_shielded_pq::tree::CAPACITY {
        return Ok(Skipped(SkipReason::TreeFull));
    }

    // Every rule holds: apply. The root about to be replaced is recorded
    // again now: an anchor's window runs from when its root stopped being
    // current, not from when it first appeared. Otherwise, after a quiet
    // stretch longer than the window, the next transaction would instantly
    // expire every proof made against the root it replaces (and strand the
    // deposits of shield-ins proven against it).
    record_root(state, &frontier.root(), height)?;
    for nf in &body.nf {
        set(state, nf_key(nf), U256::one())?;
    }
    let positions = [
        frontier.append(digest_of(&body.cm_out[0])).expect("capacity checked"),
        frontier.append(digest_of(&body.cm_out[1])).expect("capacity checked"),
    ];
    store_frontier(state, &frontier)?;
    record_root(state, &frontier.root(), height)?;
    if body.v_public_in > 0 {
        set(state, credit, credited - shielded_to_mazzy(body.v_public_in))?;
        let pending =
            pending_credits(state)?.saturating_sub(shielded_to_mazzy(body.v_public_in));
        set(state, PENDING_CREDITS_KEY.to_vec(), pending)?;
    }
    let (total_in, total_out, total_burnt) = turnstile(state)?;
    set(state, TOTAL_IN_KEY.to_vec(), total_in + shielded_to_mazzy(body.v_public_in))?;
    set(state, TOTAL_OUT_KEY.to_vec(), total_out + shielded_to_mazzy(body.v_public_out))?;
    set(state, TOTAL_BURNT_KEY.to_vec(), total_burnt + shielded_to_mazzy(body.fee))?;

    if body.v_public_out > 0 {
        breaker_record(state, height, params, v_out)?;
        let dest = body.dest.expect("stage A: a shield-out has a destination");
        pay_out(state, &dest, shielded_to_mazzy(body.v_public_out))?;
    }
    if body.fee > 0 {
        let fee = shielded_to_mazzy(body.fee);
        state.sub_balance(&backing_account(), &fee, &mut CleanupMode::NoEmpty)?;
        state.burn_by_mip1559(fee);
    }
    Ok(ShieldedOutcome::Applied { positions })
}

fn pay_out(state: &mut State, dest: &ShieldedDest, amount: U256) -> DbResult<()> {
    state.sub_balance(&backing_account(), &amount, &mut CleanupMode::NoEmpty)?;
    match dest.space {
        Space::Native => state.add_balance(
            &dest.address.with_native_space(),
            &amount,
            CleanupMode::ForceCreate,
        ),
        Space::Ethereum => {
            state.add_total_evm_tokens(amount);
            state.add_balance(
                &dest.address.with_evm_space(),
                &amount,
                CleanupMode::ForceCreate,
            )
        }
        Space::Shielded => unreachable!("stage A rejects a shielded destination"),
    }
}
