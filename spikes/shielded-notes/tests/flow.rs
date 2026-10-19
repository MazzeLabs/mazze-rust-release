//! End-to-end: what Alice sends off-chain is enough for Bob to claim, and
//! nothing else works. Run with `cargo test --release` (McEliece keygen is slow
//! in debug builds).

use shielded_notes_spike::chain::*;
use shielded_notes_spike::keys::*;
use shielded_notes_spike::note::*;
use shielded_notes_spike::primitives::*;
use shielded_notes_spike::seal;
use shielded_notes_spike::tree::DEPTH;
use shielded_notes_spike::wallet::*;

fn setup() -> (Ledger, Wallet, Wallet, Seed) {
    let mut l = Ledger::new();
    let mut alice = Wallet::new(Seed::random());
    let bob_seed = Seed::random();
    let bob = Wallet::new(bob_seed.clone());
    alice.shield_in(&mut l, 101).unwrap(); // 100 after fee
    (l, alice, bob, bob_seed)
}

#[test]
fn hiding_bound_is_far_beyond_128_bits() {
    let bits = hiding_bits();
    println!("statistical distance ≤ 2^-{bits:.0}");
    assert!(bits >= 128.0);
}

#[test]
fn plain_opening_lets_bob_claim_and_restore_from_seed() {
    let (mut l, mut alice, mut bob, bob_seed) = setup();
    let (bob_addr, _) = bob.address(0);
    // Bob publishes his address as a string; Alice pastes it.
    let pasted = Address::decode(&bob_addr.encode()).unwrap();

    let opening = alice.pay(&mut l, &pasted, 25, SendMode::RecipientBlind).unwrap();
    assert_eq!(alice.balance(&l), 100 - 25 - FEE);

    // Off-chain: a plain string (QR / file / paper).
    let qr = opening.to_plain();
    let received = NoteOpening::from_plain(&qr).unwrap();
    assert_eq!(bob.receive(&l, received).unwrap(), 25);

    // Mandatory claim: a Bob→Bob transaction that passes the spend relation.
    assert_eq!(bob.claim_all(&mut l).unwrap(), 1);
    assert_eq!(bob.balance(&l), 25 - FEE);

    // Bob loses everything except his seed.
    let restored = Wallet::restore(bob_seed, &l);
    assert_eq!(restored.balance(&l), 25 - FEE);

    // And spends it: the restored wallet produces valid witnesses.
    let mut restored = restored;
    let (carol, _) = Wallet::new(Seed::random()).address(0);
    restored.pay(&mut l, &carol, 10, SendMode::NoTrace).unwrap();
    assert_eq!(restored.balance(&l), 25 - FEE - 10 - FEE);
}

#[test]
fn sealed_opening_over_any_channel() {
    let (mut l, mut alice, mut bob, _) = setup();
    let (bob_addr, bob_bundle) = bob.address(1);
    let opening = alice.pay(&mut l, &bob_addr, 40, SendMode::NoTrace).unwrap();

    let blob = seal::seal(&bob_addr, &bob_bundle, &opening).unwrap();
    println!("sealed blob: {} bytes, bundle: {} bytes", blob.len(), bob_bundle.0.len());

    // A third party cannot open it.
    let eve = Wallet::new(Seed::random());
    assert!(eve.open_sealed(&blob).is_none());

    // Tampering is detected.
    let mut bad = blob.clone();
    *bad.last_mut().unwrap() ^= 1;
    assert!(bob.open_sealed(&bad).is_none());

    let got = bob.open_sealed(&blob).unwrap();
    assert_eq!(bob.receive(&l, got).unwrap(), 40);
    bob.claim_all(&mut l).unwrap();
    assert_eq!(bob.balance(&l), 40 - FEE);
}

#[test]
fn sealing_rejects_a_bundle_that_does_not_match_the_address() {
    let (_, _, bob, _) = setup();
    let (addr0, _) = bob.address(0);
    let (_, other_bundle) = bob.address(2);
    let fake = NoteOpening { d: addr0.d, value: 1, psi: random_fields(), y: random_fields() };
    assert!(seal::seal(&addr0, &other_bundle, &fake).is_err());
}

#[test]
fn alice_cannot_spend_the_note_she_created_for_bob() {
    let (mut l, mut alice, _, _) = setup();
    let bob = Wallet::new(Seed::random());
    let (bob_addr, _) = bob.address(0);
    let opening = alice.pay(&mut l, &bob_addr, 25, SendMode::RecipientBlind).unwrap();

    // Alice knows the full opening and the owner tag, but not Bob's nsk.
    let note = opening.clone().into_note(bob_addr.owner);
    let pos = l.tree.find(&note.cm()).unwrap();
    let theft_out = Note { d: alice.seed.diversifier(0), owner: alice.keys.owner(&alice.seed.diversifier(0)), value: 25 - FEE, psi: random_fields(), y: random_fields() };
    let dummy_out = Note { d: random_fields(), owner: random_fields(), value: 0, psi: random_fields(), y: random_fields() };
    let dummy_in = InputWitness { nsk: random_fields(), note: NoteOpening { d: random_fields(), value: 0, psi: random_fields(), y: random_fields() }, pos: 0, path: [[p3_field::PrimeCharacteristicRing::ZERO; DIGEST]; DEPTH] };
    let try_with = |nsk: Digest| {
        let k = SpendKeys::from_nsk(nsk);
        let w = Witness {
            inputs: [InputWitness { nsk, note: opening.clone(), pos, path: l.tree.path(pos) }, dummy_in.clone()],
            outputs: [theft_out.clone(), dummy_out.clone()],
        };
        let dk = SpendKeys::from_nsk(dummy_in.nsk);
        let tx = ShieldedTx {
            proof_system_id: PS_1,
            anchor: l.tree.root(),
            nf: [nullifier(&k.nk, &opening.psi, pos), nullifier(&dk.nk, &dummy_in.note.psi, 0)],
            cm_out: [theft_out.cm(), dummy_out.cm()],
            recovery: Recovery::random(),
            v_public_in: 0, v_public_out: 0, dest: None, fee: FEE,
            proof: Proof::Simulated { sighash: [0; 32], witness: w.clone() },
        };
        check_relation(&tx, &w)
    };
    // With her own key: the recomputed owner differs, so the leaf isn't in the tree.
    let err = try_with(alice.keys.nsk).unwrap_err();
    assert!(err.contains("not in the tree"), "{err}");
    // With a random key: same.
    assert!(try_with(random_fields()).is_err());
    // Positive control: the identical witness with Bob's key is accepted, so the
    // rejections above are about the key, not a broken harness.
    try_with(bob.keys.nsk).expect("Bob's own key must satisfy the relation");
}

#[test]
fn forged_or_altered_openings_are_rejected_by_bob() {
    let (mut l, mut alice, mut bob, _) = setup();
    let (bob_addr, _) = bob.address(0);
    let opening = alice.pay(&mut l, &bob_addr, 25, SendMode::RecipientBlind).unwrap();

    let mut more = opening.clone();
    more.value = 2500; // Alice claims she sent more
    assert!(bob.receive(&l, more).unwrap_err().contains("not on chain"));

    let mut other = opening.clone();
    other.y[0] += <F as p3_field::PrimeCharacteristicRing>::ONE;
    assert!(bob.receive(&l, other).is_err());

    // Not addressed to Bob at all.
    let mut foreign = opening.clone();
    foreign.d = random_fields();
    assert!(bob.receive(&l, foreign).unwrap_err().contains("not for any of my addresses"));

    // Malformed / non-canonical encodings.
    assert!(NoteOpening::from_plain("mzn1abc").is_none());
    let mut bytes = opening.to_bytes();
    bytes[0] = 9;
    assert!(NoteOpening::from_bytes(&bytes).is_none());
}

#[test]
fn double_claim_and_double_spend_are_rejected() {
    let (mut l, mut alice, mut bob, _) = setup();
    let (bob_addr, _) = bob.address(0);
    let opening = alice.pay(&mut l, &bob_addr, 25, SendMode::RecipientBlind).unwrap();
    bob.receive(&l, opening.clone()).unwrap();
    bob.claim_all(&mut l).unwrap();
    // The same opening again: Bob's wallet sees its nullifier spent.
    assert!(bob.receive(&l, opening).unwrap_err().contains("already spent"));
    // Replaying a whole transaction is rejected by the ledger.
    let replay = l.txs.last().unwrap().clone();
    assert!(l.apply(replay).unwrap_err().contains("double spend"));
}

#[test]
fn recipient_blind_history_and_resend_after_alice_restores() {
    let (mut l, mut alice, mut bob, _) = setup();
    let alice_seed = alice.seed.clone();
    let (bob_addr, _) = bob.address(0);
    let (carol_addr, _) = Wallet::new(Seed::random()).address(0);
    alice.pay(&mut l, &bob_addr, 25, SendMode::RecipientBlind).unwrap();
    alice.pay(&mut l, &carol_addr, 7, SendMode::NoTrace).unwrap();
    // Bob never received his opening. Alice wipes her device.
    drop(alice);

    let alice = Wallet::restore(alice_seed, &l);
    let hist = alice.history(&l);
    // History shows amounts, not recipients.
    assert_eq!(hist.iter().map(|e| e.amount).collect::<Vec<_>>(), vec![25, 7]);

    // "Ah, the 25 was for Bob": the wallet checks the guess against the chain.
    let wrong = alice.resend(&l, &hist[0], &carol_addr);
    assert!(wrong.is_err());
    let again = alice.resend(&l, &hist[0], &bob_addr).unwrap();
    // The no-trace payment cannot be regenerated, by design.
    assert!(alice.resend(&l, &hist[1], &carol_addr).is_err());

    assert_eq!(bob.receive(&l, again).unwrap(), 25);
    bob.claim_all(&mut l).unwrap();
    assert_eq!(bob.balance(&l), 25 - FEE);
}

#[test]
fn turnstile_and_shield_out() {
    let (mut l, mut alice, _, _) = setup();
    assert_eq!(l.backing, 100);
    alice.shield_out(&mut l, [7u8; 20], 30).unwrap();
    assert_eq!(l.backing, 100 - 30 - FEE);
    assert_eq!(l.backing, alice.balance(&l)); // the pool holds exactly what notes are worth
}

#[test]
fn every_transaction_has_the_same_public_shape() {
    let (mut l, mut alice, mut bob, _) = setup();
    let (bob_addr, _) = bob.address(0);
    let o = alice.pay(&mut l, &bob_addr, 25, SendMode::RecipientBlind).unwrap();
    bob.receive(&l, o).unwrap();
    bob.claim_all(&mut l).unwrap();
    // shield-in, payment, claim: same body size, standard fee, a recovery slot each.
    let sizes: Vec<usize> = l.txs.iter().map(|t| t.body_len()).collect();
    assert!(sizes.windows(2).all(|w| w[0] == w[1]));
    assert!(l.txs.iter().all(|t| t.fee == FEE));
}

#[test]
fn notes_survive_proof_system_hardforks() {
    let (mut l, mut alice, mut bob, bob_seed) = setup();
    let (bob_addr, _) = bob.address(0);
    // Note created and claimed under PS-1.
    let o = alice.pay(&mut l, &bob_addr, 25, SendMode::RecipientBlind).unwrap();
    bob.receive(&l, o).unwrap();
    bob.claim_all(&mut l).unwrap();
    assert!(l.txs.iter().all(|t| t.proof_system_id == PS_1));

    // Hardfork 1: PS-2 added and made the default; PS-1 still allowed.
    l.hardfork_proof_systems(&[PS_1, 2], 2);
    let (carol, _) = Wallet::new(Seed::random()).address(0);
    bob.pay(&mut l, &carol, 5, SendMode::NoTrace).unwrap();
    assert_eq!(l.txs.last().unwrap().proof_system_id, 2, "PS-1 note spent with a PS-2 proof");

    // Hardfork 2: PS-1 removed. A PS-1 transaction is rejected...
    l.hardfork_proof_systems(&[2], 2);
    let mut old = l.txs.last().unwrap().clone();
    old.proof_system_id = PS_1;
    assert!(l.apply(old).unwrap_err().contains("not allowed"));
    // ...but nothing is stuck: everything (including notes from the PS-1 era) still spends.
    let mut restored = Wallet::restore(bob_seed, &l);
    let bal = restored.balance(&l);
    restored.pay(&mut l, &carol, bal - FEE, SendMode::NoTrace).unwrap();
    assert_eq!(restored.balance(&l), 0);
}

#[test]
fn proof_system_id_is_bound_to_the_proof() {
    let (mut l, mut alice, bob, _) = setup();
    l.hardfork_proof_systems(&[PS_1, 2], PS_1);
    let (bob_addr, _) = bob.address(0);
    alice.pay(&mut l, &bob_addr, 25, SendMode::RecipientBlind).unwrap();
    // Re-labelling a valid proof as another (allowed) proof system breaks the binding.
    let mut relabelled = l.txs.last().unwrap().clone();
    relabelled.proof_system_id = 2;
    relabelled.nf = [random_fields(), random_fields()]; // avoid the double-spend check firing first
    let err = l.apply(relabelled).unwrap_err();
    assert!(err.contains("not bound"), "{err}");
}

#[test]
fn claim_consolidates_so_one_note_pays_everything() {
    let mut l = Ledger::new();
    let mut bob = Wallet::new(Seed::random());
    let (bob_addr, _) = bob.address(0);
    // Five different payers, five separate receipts.
    let mut expected = 0;
    for amount in [10u64, 20, 30, 40, 50] {
        let mut payer = Wallet::new(Seed::random());
        payer.shield_in(&mut l, amount + 2).unwrap();
        let o = payer.pay(&mut l, &bob_addr, amount, SendMode::RecipientBlind).unwrap();
        bob.receive(&l, o).unwrap();
        bob.claim_all(&mut l).unwrap();
        expected += amount - FEE;
        // After every claim Bob holds exactly one spendable note.
        assert_eq!(bob.spendable(&l).len(), 1, "wallet did not consolidate");
        assert_eq!(bob.balance(&l), expected);
    }
    // So a payment of almost everything fits in a single 2×2 transaction.
    let (carol, _) = Wallet::new(Seed::random()).address(0);
    bob.pay(&mut l, &carol, expected - FEE, SendMode::NoTrace).unwrap();
    assert_eq!(bob.balance(&l), 0);
}
