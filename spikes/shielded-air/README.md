# Spike M1 / M1.5: the spend statement as a real Plonky3 AIR (PS-1)

This spike implements the per-input part of the spend statement (spec §7) as a real AIR with zero-knowledge proofs. It is **PS-1**:
- KoalaBear with the degree-8 extension;
- FRI;
- Keccak-256 for commitments and Fiat–Shamir;
- hiding FRI with salted Merkle leaves;
- **128-bit proven** soundness, with the query count chosen by `p3-security`.

```bash
cargo test --release -- --nocapture          # all tests; prints proof sizes
BREAKDOWN=1 cargo test --release --test m15 -- --nocapture size   # per-component bytes
```

**Statement.** The prover knows `(d ‖ b)`, a path, a position `pos`, `nk` and `ψ` such that:
- `cm = H_LEAF(d ‖ b)`;
- `anchor = Root(cm, pos, path)`, a public value;
- `nf = H_NF(H_MK(nk ‖ ψ) ‖ pos_lo16 ‖ pos_hi16)`, a public value.

M2–M4 add the HM commitment opening (`A·y`), the owner and key derivation, the outputs, ranges and the balance, and bind them together.

## Two layouts

| | `input_air` (M1) | `narrow_air` (M1.5) |
|---|---|---|
| Layout | one Poseidon2-24 permutation per row (stock `p3-poseidon2-air` via `SubAirBuilder`) | **one round per row**: 32 rows per permutation; round constants and selectors are periodic columns |
| Width × height | 254 × 512 | **39 × 2048** |
| Max constraint degree | 3 | 4 |

Both layouts are checked against the native implementation in `spikes/shielded-notes`: the same root and nullifier for positions 0 … 2^32−1, and every round state equals the native permutation. Both reject every tampering class in the tests (13 for M1, 14 for M1.5), and neither verifies against forged public values.

## Results (16-core desktop, 2026-10-19)

ZK, 128-bit proven, arity 8, 16-bit query grinding:

| Layout | Blowup | Queries | Proof | Prove | Verify |
|---|---:|---:|---:|---:|---:|
| M1 | 16 | 57 | 226 KB | 0.1 s | 24 ms |
| M1 | 256 | 29 | 157 KB | 1.1 s | 20 ms |
| **M1.5** | 16 | 57 | 195 KB | 0.2 s | 6 ms |
| **M1.5** | 64 | 38 | 155 KB | 0.7 s | 5 ms |
| **M1.5** | 256 | 29 | **137 KB** | 3.8 s | **4–6 ms** |

**Where the bytes go** (M1.5, blowup 256):

| Component | KB |
|---|---:|
| Merkle multiproofs (3 opening trees + 4 FRI layers, 32-byte Keccak digests) | **~81 (59%)** |
| Quotient values (16 chunks × degree-8 extension per query) | 15 |
| FRI sibling values | 26 |
| Trace values (39 columns) | 5 |
| OOD values and other | ~9 |

**Proof size vs trace height** (same width, blowup 256):

| Height | Proof |
|---:|---:|
| 2048 | 137 KB |
| 4096 | 149 KB |
| 8192 | 160 KB |

Every doubling adds about 11 KB, and proving time grows linearly.

## M2: one input bound end to end, plus one output (`tx_air`)

`nsk → nk, ak → owner → x = owner ‖ limbs(value) ‖ ψ → HM opening (H_HM(y), x − A·y) → leaf → Merkle → nf`, plus an output's HM opening to `cm_out`. The narrow layout is 107 columns × 2048 rows (62 permutations).

**Binding devices:**
- *Registers* `nk`, `owner`, `ψ`, `value` are held constant over a note's rows, and pinned where they are produced or consumed.
- The nk permutation's chunk (`nsk`) is *carried* into the ak permutation, so both derive from the same `nsk`.
- `A·y` is accumulated one `y_j` per row, with the matrix coefficients as periodic columns. Each leaf `b` element is checked as `x − A·y`.

**Tests (`tests/m2.rs`):**
- the trace matches the native wallet (commitments, root, nullifier);
- **18 binding violations are rejected**, including consistent whole-region register lies, a different `nsk` for `ak` than for `nk`, `y` changed mid-permutation, and the `A·y` accumulator;
- *Alice can't spend Bob's note:* with Bob's full opening and her `nsk`, she proves a different leaf, which fails against the real anchor; Bob's key passes on the same note;
- a forged `cm_out` doesn't verify.

| Blowup | Proof | Prove | Verify |
|---:|---:|---:|---:|
| 64 | 170 KB | 0.8 s | 5.9 ms |
| 256 | **150 KB** | 3.0 s | **4.4 ms** |

The 68 extra columns cost about 13 KB. Values are cheap; Merkle paths dominate.

## M3 + M4: the complete 2-in × 2-out statement (`tx2_air`)

The layout is 118 columns × 4096 rows (123 permutations): inputs A and B run the M2 chain, and outputs C and D run HM → leaf.

**New pieces:**
- **Ranges (C8):** every 16-bit limb (4 per note, plus the 12 public limbs of `v_in`/`v_out`/`fee`) is decomposed bit by bit over 16 rows, most significant bit first. Carries get 3-bit blocks.
- **Balance (C9):** limb-wise, `A + B + v_in = C + D + v_out + fee`, with carries in [−4, 3] and the final carry forced to 0. All terms are ≪ p/2, so field equality implies integer equality.
- **Dummy gate (C4):** `Z = [value ≠ 0]` comes from an inverse gadget, not a free bit. Membership is enforced only when `Z = 1`.
- **Sighash (C10):** a public input bound by Fiat–Shamir.

**Tests (`tests/m4.rs`), each attack checked at the exact row of the defence it targets:**
- **Honest shapes accepted, and the public values match the native wallet:** a payment (1 real input + 1 dummy), two real inputs, a shield-in, a shield-out, and values near 2^64 that carry through every limb.
- **Inflation by +1:** caught by the balance row, for any choice of forged carries.
- **Consistent wraparound:** output C has limb −1 in its commitment, its `b` elements and its register; output D compensates, so the balance holds mod p. It is caught by **C's limb-0 range-check row**.
- **Real input not in the tree:** caught at the anchor-check row.
- **Forged `Z = 0` to skip membership:** caught by the gadget on the region's first row.
- **Also rejected:** out-of-range carries, a wrong public fee, a public limb ≥ 2^16, Alice using Bob's note (Bob's key passes), and swapped nullifiers.
- **120 random state-cell mutations:** all rejected.
- **Sighash:** the proof doesn't verify under another sighash.

| Blowup | Proof | Prove | Verify |
|---:|---:|---:|---:|
| 64 | ~180 KB | 1.7 s | 5.8 ms |
| 256 | **~165 KB** | 6.3 s | **6.1 ms** |

These are the **final PS-1 numbers for a whole transaction**. Sizes vary by a few KB between runs because of ZK randomness.

**Not done yet (needed before production):**
- Fiat–Shamir domain separation: the challenger should start from `(proof_system_id, CHAIN_ID, AIR id)`. Today it starts empty.
- A2/A3: an SMT under-constraint check and formal proofs of N-UNIQ and balance.
- A8: an external audit.
- Wallet-side proving on target hardware.
- Consensus integration (P2–P4).

## Conclusions

1. **The narrow layout wins on verification (4× faster) more than on size (−12%).** Once the trace is narrow, the proof is dominated by Merkle authentication paths, which are the cost of 256-bit digests at 128-bit proven soundness. Merkle caps don't help, because Plonky3 already deduplicates shared path nodes in its multi-openings.
2. **The full transaction costs little more than one input.** The 2×2 statement (about 122 permutations plus `A·y` and range rows) needs 4096–8192 rows, so we expect **a full PS-1 proof of about 150–160 KB per transaction**. That is not 4× the single-input size.
3. **Proving time is the real knob for wallets:** about 4–15 s at blowup 256, against about 1–4 s at blowup 64 (where proofs are about 20 KB larger), measured on a desktop.
4. Shrinking further needs a **different proof system**, not tuning: for example WHIR, or conjectured instead of proven soundness at about half the size. That is a PS-2/PS-3 question (spec §6.4). Block space is not affected: blocks carry only the ~400-byte transaction bodies.
