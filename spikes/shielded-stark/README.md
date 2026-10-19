# P0 spike: STARK proof sizing for the Shielded Space

This spike answers one question from [privacy-pq-stark.md §11](../../docs/internal/privacy-pq-stark.md): **how large, and how fast to verify, is a 128-bit STARK proof for the 2-in × 2-out shielded statement?**

The statement is dominated by Poseidon2 permutations:
- each input needs 5 hashes plus 32 Merkle nodes, 37 in total;
- each output needs 1 hash;
- 2×37 + 2×1 = 76, which rounds up to **128 permutations**.

The spike proves 128 and 256 Poseidon2 permutations (KoalaBear, width 16) with Plonky3 0.8's stock AIR. For each FRI shape it searches for the minimum number of queries that reaches 128 bits, scored by `p3-security` under two notions:
- **conjectured:** the random-words formula, ePrint 2025/2010;
- **proven:** the better of the unique-decoding and list-decoding bounds.

This is a sizing estimate. The real AIR adds range-check and balance columns, which will change the numbers by a few KB.

```bash
cargo run --release                     # challenge field KoalaBear^8 (248 bits) — default
cargo run --release --features vec2     # 2 perms per row (2× wider, ½ height) — width study
cargo run --release --features vec4     # 4 perms per row
cargo run --release --features ext5     # KoalaBear^5 (155 bits)
cargo run --release --features ext4     # KoalaBear^4 (124 bits)
```

It needs Rust 1.93 or later, pinned in `rust-toolchain.toml`. The node workspace pins 1.91, so integrating Plonky3 0.8 requires a toolchain bump (a P1 item).

## Results (128 permutations, 16-core desktop, 2026-10-08)

Commitments use Keccak-256 Merkle trees, and query-phase grinding is 16 bits unless noted. Every row reaches 128 bits under the notion in its "regime" column.

### KoalaBear^4 (124-bit challenge field): **cannot reach 128 bits**

Soundness plateaus at **105–108 bits** under both notions, however many queries are used. The binding terms depend on field size, so adding queries doesn't help. **This field is rejected.**

### KoalaBear^5 (155 bits)

| blowup | arity | regime | queries | proof KB | verify ms |
|---:|---:|---|---:|---:|---:|
| 8 | 8 | conjectured | 38 | 53.2 | 0.71 |
| 16 | 8 | conjectured | 29 | 45.9 | 0.69 |
| 4 | 2 | proven | 135 | 135.0 | 1.59 |
| 16 | 8 | proven | 123 | 159.5 | 1.97 |

### KoalaBear^8 (248 bits): **recommended**

| blowup | arity | pow | regime | queries | proof KB | prove ms | verify ms |
|---:|---:|---:|---|---:|---:|---:|---:|
| 8 | 8 | 16 | conjectured | 38 | 65.0 | 4.9 | 1.01 |
| 16 | 8 | 16 | conjectured | 29 | 54.8 | 4.4 | 0.85 |
| 8 | 8 | 16 | proven | 75 | 113.4 | 6.2 | 1.56 |
| **16** | **8** | **16** | **proven** | **57** | **94.8** | **7.1** | **1.31** |
| 16 | 8 | 24 | proven | 53 | 90.1 | 589.7 | 1.71 |

### Trace width (KoalaBear^8, blowup 16, arity 8): **width is the cost driver**

| perms/row | width (columns) | height (rows) | conjectured KB | proven KB |
|---:|---:|---:|---:|---:|
| 1 | 164 | 128 | 54.8 | 94.8 |
| 2 | 328 | 64 | 74.7 | 128.3 |
| 4 | 656 | 32 | 115.4 | 200.1 |

Each extra trace column costs about **0.2 KB proven** (57 queries × 4 bytes) and about **0.12 KB conjectured**. Rows cost almost nothing, because size grows with the log of the height. **Design rule for the real AIR: keep it narrow and tall.** That means one hash per row, Merkle levels laid out as successive rows, and range-check limbs spread across rows rather than placed side by side in columns. As a rough budget, ~40 columns beyond the Poseidon2 core puts the proven proof at about 103 KB.

The 256-permutation rows are a few KB larger in every configuration. Proof size grows with the logarithm of trace height, so a deeper tree or extra columns cost little.

## What this means

1. **Verification time is not a constraint.** It is 1–2 ms per proof, 15× under the 30 ms gate. Proving time is also negligible at this size; the real AIR's witness generation will dominate, still well under 1 s.
2. **The field choice is settled: an extension of degree ≥ 8.** The 124-bit extension that Plonky3 examples use by default cannot reach 128-bit soundness for this statement.
3. **Proof size depends on the soundness notion:**
   - **Proven 128 bits ≈ 95 KB**, which fails the ~80 KB gate. A full transaction (proof plus 2 outputs × ~1.8 KB of ciphertexts) comes to about 99 KB. In a 200 KB block that leaves room for **1 shielded tx per block (~1 tx/s)**.
   - **Conjectured 128 bits ≈ 55 KB**, which passes the gate: a ~59 KB transaction and 2–3 per block. This is what most production STARK deployments use. It rests on Reed–Solomon proximity conjectures, and some of the stronger "up-to-capacity" conjectures were weakened by 2025 results (which is why `p3-security` now uses the more conservative random-words formula).
4. **Quantum margin:** against a quantum adversary, Grover search over Fiat–Shamir transcripts gives a quadratic speedup. 128 classical bits is therefore roughly NIST category 1, the same margin as AES-128. Going higher means more queries, about +40% proof size for +32 bits.

## Decision needed (for the owner)

| Option | Tx size | Shielded tx/s at 200 KB blocks | Soundness basis |
|---|---:|---:|---|
| A. Proven 128 bits | ~99 KB | ~1 | Proven |
| B. Conjectured 128 bits | ~59 KB | ~2–3 | Conjecture (random-words) |
| C. Proven 128 bits + separate shielded byte budget per block | ~99 KB | tunable | Proven |

Recommendation: **A at launch, with C as the throughput lever.** For a pool whose failure mode is silent inflation (lesson Z-1 from Zcash), it's worth paying ~40 KB per transaction for proven soundness. Throughput can be raised later with a dedicated shielded byte budget or recursive aggregation (v2), without weakening soundness.

### Security level and quantum margin (KoalaBear^8, blowup 16, arity 8)

```bash
TARGET_BITS=192 cargo run --release --features digest512
```

With 256-bit Keccak commitments, **every configuration stops at exactly 128 bits**. The commitment hash's collision resistance is the binding term, and adding queries doesn't move it. Against a quantum adversary that term is weaker still: BHT collision search costs ~2^(n/3), which is about 85 bits for a 256-bit digest. Going beyond 128 bits therefore needs 512-bit commitment digests (`--features digest512`):

| digest | target | regime | queries | proof KB | verify ms |
|---:|---:|---|---:|---:|---:|
| 256 | 128 | proven | 57 | 95.1 | 1.3 |
| 512 | 128 | proven | 57 | 118.3 | 1.4 |
| 512 | 160 | proven | 73 | 142.9 | 1.5 |
| 512 | **192** | **proven** | **89** | **164.3** | **1.8** |
| 512 | 256 | proven | — | unreachable (maxes at ~229 bits; the field limits it) | — |
| 512 | 192 | conjectured | 45 | 96.8 | 1.1 |

**Rule of thumb for quantum margin.** A Fiat–Shamir STARK's soundness against a quantum adversary is roughly *half* its classical round-by-round bits: the QROM bound is about t²·ε for t quantum queries. So 128 classical bits gives roughly 64 quantum bits, and 192 classical bits gives roughly 96. Collision resistance also falls to n/3 bits. Both point to **192-bit proven soundness with 512-bit digests (~164 KB)** as the configuration with a real quantum margin.

### Zero-knowledge mode: **mandatory, and it roughly doubles proof size**

```bash
PERMS=512 MIN_LOG_BLOWUP=4 TARGET_BITS=128 cargo run --release --features zk
```

**Every number above was measured without ZK.** A plain STARK proof opens trace-polynomial evaluations, and those leak information about the witness: secret keys, values, Merkle paths. The shielded space must use the hiding variant, which salts the Merkle leaves (`MerkleTreeHidingMmcs`) and adds random codewords to FRI (`HidingFriPcs`, 8 codewords, at least the extension degree). The hiding budget requires trace height > `queries × extension degree`, so the 128-permutation trace has to be **padded to 512 rows**. Rows are cheap, so the padding costs little.

| digest | target | regime | trace rows | proof KB | prove ms | verify ms |
|---:|---:|---|---:|---:|---:|---:|
| 256 | 128 | conjectured | 512 | 108.3 | 62 | 1.7 |
| 256 | 128 | **proven** | 512 | **191.1** | 70 | 2.7 |
| 512 | 128 | proven | 512 | 255.6 | 63 | 2.8 |
| 512 | 192 | conjectured | 512 | 208.7 | 67 | 2.3 |
| 512 | 192 | proven | 512 | 363.2 | 72 | 4.1 |

**Larger blowup shrinks ZK proofs** (fewer queries; prover cost grows with blowup, but our trace is tiny). ZK, 256-bit digests, 128-bit target, 512 rows, arity 8, pow 16:

| blowup | conjectured KB | proven KB | prove ms (proven) | verify ms |
|---:|---:|---:|---:|---:|
| 16 | 108.3 | 192.4 | 71 | 2.9 |
| 32 | 95.6 | 165.1 | 109 | 2.4 |
| 64 | 85.2 | 149.3 | 175 | 2.0 |
| 128 | 83.3 | 140.3 | 360 | 1.7 |
| 256 | 79.3 | 137.9 | 721 | 1.8 |
| 256 (arity 16, pow 20) | 77.7 | 135.1 | 720 | 1.9 |

Returns diminish past blowup 64–128. Next levers, not yet measured: a WHIR PCS (`p3-whir` 0.9.0-rc.1 exists), Ligero-style commitments (smaller than FRI for polynomials of degree ≲ 2^18, per Thaler), and the real AIR's width.

So a ZK proof at proven 128 bits is ~191 KB at blowup 16 (~138 KB at blowup 256) per transaction, almost a whole 200 KB block. **Per-transaction proofs on-chain are not viable. Block-level aggregation (the "Shadow Chain") is required.**

## Prover choice: Plonky3

The Stwo comparison is in [../shielded-stark-stwo](../shielded-stark-stwo/README.md). Stwo is rejected for three reasons:
- releases 2.x can't prove degree ≥ 2 AIRs;
- its 124-bit challenge field is fixed;
- at the same width its proofs are larger (~71 KB under its optimistic score) and slower to produce.

Still open: re-measuring on the real AIR once P1 exists.
