//! Walk-through with real sizes: Alice pays Bob, delivers off-chain, Bob claims,
//! Bob restores from his seed alone.

use std::time::Instant;

use shielded_notes_spike::chain::*;
use shielded_notes_spike::keys::*;
use shielded_notes_spike::note::*;
use shielded_notes_spike::seal;
use shielded_notes_spike::wallet::*;

fn main() {
    println!("commitment hiding: statistical distance <= 2^-{:.0}", hiding_bits());

    let mut l = Ledger::new();
    let mut alice = Wallet::new(Seed::random());
    let bob_seed = Seed::random();
    let mut bob = Wallet::new(bob_seed.clone());

    let t = Instant::now();
    let (addr, bundle) = bob.address(0);
    println!("address + bundle derivation: {:.0} ms (McEliece-460896 keygen)", t.elapsed().as_secs_f64() * 1e3);
    let addr_s = addr.encode();
    println!("address: {} chars  {}…", addr_s.len(), &addr_s[..40]);
    println!("bundle (off-chain, once per relationship): {} bytes", bundle.0.len());

    alice.shield_in(&mut l, 101).unwrap();
    println!("\nAlice shield-in 101 → balance {} (fee {} burnt)", alice.balance(&l), FEE);

    let opening = alice.pay(&mut l, &addr, 25, SendMode::RecipientBlind).unwrap();
    println!("Alice pays Bob 25 → Alice balance {}", alice.balance(&l));
    println!("  on-chain body: {} bytes per transaction (proof goes to aggregation)", l.txs[1].body_len());

    let plain = opening.to_plain();
    println!("  plain opening: {} chars (QR / file / paper)", plain.len());
    let blob = seal::seal(&addr, &bundle, &opening).unwrap();
    println!("  sealed opening: {} bytes (any channel; anonymous, no sender id)", blob.len());

    let t = Instant::now();
    let got = bob.open_sealed(&blob).expect("Bob opens it");
    println!("\nBob opens sealed blob in {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    println!("Bob verifies on-chain → received {}", bob.receive(&l, got).unwrap());
    bob.claim_all(&mut l).unwrap();
    println!("Bob claims (Bob→Bob tx passes the spend relation) → balance {}", bob.balance(&l));

    drop(bob);
    let restored = Wallet::restore(bob_seed, &l);
    println!("Bob restores from seed only → balance {}", restored.balance(&l));

    println!("\nAlice history (no recipients): {:?}", alice.history(&l).iter().map(|e| e.amount).collect::<Vec<_>>());
    println!("pool backing {} = sum of all notes; burnt {}", l.backing, l.burnt);
}
