# Spike: statistically hiding commitments, off-chain delivery, claim, restore

This spike answers two questions from [privacy-pq-stark.md](../../docs/internal/privacy-pq-stark.md) §4.2 and §9:
1. Can note commitments be **statistically** hiding, so that no future mathematics can reveal what's inside them?
2. Is what Alice sends off-chain **enough for Bob to claim** the note, and to recover it from his seed alone?

```bash
cargo test --release   # 10 end-to-end tests (McEliece keygen is slow in debug builds)
cargo run --release    # walk-through with real sizes
```

It uses Rust 1.93 (pinned) and real cryptography throughout: Poseidon2 over KoalaBear (Plonky3), SHAKE256, ML-KEM-768, Classic McEliece-460896, X25519 and ChaCha20-Poly1305.

**What is not real:** the STARK proof. `Proof::Simulated` carries the witness, and the ledger checks it with `check_relation`, which is the same statement the AIR will enforce (C1–C9). The ledger never looks at the witness except through that function. So "Bob builds a witness that passes `check_relation`" is equivalent to "Bob can produce a valid proof".

## The commitment (Halevi–Micali over KoalaBear)

```
x  = owner(12) ‖ value limbs(4) ‖ ψ(12)     T = 28 field elements
y  ← F^64, exactly uniform (rejection sampling)
d  = Poseidon2(y)        b = x − A·y        cm = Poseidon2(d ‖ b)
```

- **Hiding is statistical.** `A·y` is a universal hash of `y`. `y` carries about 1984 bits and `d` leaks at most 372 of them, so by the leftover hash lemma `b` is within **2^-372** of uniform, independently of `x`. **Privacy does not depend on Poseidon2 at all.**
- **Binding is computational.** Breaking it needs a Poseidon2 collision. That would allow only forward counterfeiting (upgradeable, bounded by the turnstile), never a reveal of past notes.
- **The caveat.** The guarantee is literally statistical only for fresh OS randomness (no-trace mode). For seed-derived notes (recipient-blind payments, self-notes) it rests on SHAKE256 as a PRF, the symmetric floor, and not on any structured problem.

## Two design changes forced by the spike

1. **Per-note nullifier key.** The old nullifier was `nf = H(nk, ρ, pos)` with one `nk` per user. An adversary who could invert the hash could solve jointly across all of one user's spends and link them. Now:
   - `mk = H(nk, ψ)`, with a fresh `ψ` per note hidden in `cm`;
   - `nf = H(mk, pos)`.

   Each nullifier is independent. Tachyon converged on the same construction.
2. **`ρ` is gone.** Uniqueness comes from fresh `y`/`ψ` and from `pos` in the nullifier. The old constraint C6 (`ρ_out = nf_in`) is removed.

## Results

| What | Measured |
|---|---|
| Hiding bound | statistical distance ≤ 2^-372 |
| Address | 153 characters (`d`, `owner`, `H(bundle)`); enough to create a payment |
| Bundle (ML-KEM-768 + McEliece-460896 + X25519) | 525,377 bytes, off-chain, once per relationship |
| Bundle derivation from the seed (McEliece keygen) | ~190 ms |
| Plain opening | 524 characters (QR code, file, paper) |
| Sealed opening | 1,650 bytes (any channel; anonymous; key-committing) |
| Opening a sealed blob | ~210 ms (dominated by key re-derivation; cacheable) |
| Transaction body on-chain, excluding the proof | 398 bytes (every transaction type is identical) |

The 13 tests in [tests/flow.rs](tests/flow.rs):

| Test | Shows |
|---|---|
| `plain_opening_lets_bob_claim_and_restore_from_seed` | paste the address, pay, deliver as a string, claim, wipe, restore from seed, spend |
| `sealed_opening_over_any_channel` | Eve can't open the blob; tampering is detected; Bob opens it and claims |
| `sealing_rejects_a_bundle_that_does_not_match_the_address` | a swapped bundle is caught via `H(bundle)` |
| `alice_cannot_spend_the_note_she_created_for_bob` | Alice's key and a random key both fail, and **Bob's key passes on the same witness** (positive control) |
| `forged_or_altered_openings_are_rejected_by_bob` | an inflated value, altered randomness, a foreign address or a malformed encoding are all rejected |
| `double_claim_and_double_spend_are_rejected` | nullifier set; replay of a whole transaction |
| `recipient_blind_history_and_resend_after_alice_restores` | Alice restored from seed sees amounts only; a wrong guessed recipient is refused; the right one regenerates the opening; no-trace payments can't be regenerated |
| `claim_consolidates_so_one_note_pays_everything` | five receipts from five payers leave Bob with exactly one spendable note after each claim, and he spends almost everything in one 2×2 transaction |
| `notes_survive_proof_system_hardforks` / `proof_system_id_is_bound_to_the_proof` | notes created under PS-1 are spent under PS-2; PS-1 transactions are rejected once it is removed; relabelling a proof's system is rejected |
| `turnstile_and_shield_out` | backing equals the sum of the notes |
| `every_transaction_has_the_same_public_shape` | shield-in, payment and claim are indistinguishable by size and fee |
| `hiding_bound_is_far_beyond_128_bits` | the parameter check |

## Circuit cost of the new commitment

Per note:
- the HM digest of 64 elements takes 6 Poseidon2-24 permutations;
- the leaf of 40 elements takes 4;
- the per-note `mk` adds 1;
- the linear `A·y` term is 28 × 64 multiplications, which is negligible.

The 2×2 statement then needs about 114 permutations (it was 76). That is still under the 128 that the prover spike measured, but with width-24 Poseidon2 instead of width-16. **Next step:** re-run the proof-size measurement with this exact AIR shape.
