# P0 spike: Stwo (Circle STARK over M31) sizing

This is the Stwo side of the prover comparison. The Plonky3 side, and the counting that turns the 2-in × 2-out statement into 128 Poseidon2 permutations, are in [../shielded-stark](../shielded-stark/README.md).

```bash
SWEEP=1:1,2:1,3:1,4:1 LOG_N_INSTANCES=7 cargo run --release   # SWEEP = log_blowup:fold_step,...
```

## Version constraint (finding 1)

**Stwo 2.0 through 2.3 cannot prove degree ≥ 2 AIRs**, and Poseidon2 is one. The lifted-protocol migration disabled them: upstream marks its own Poseidon test `#[ignore = "AIRs with constraint degree >= 2 are not supported yet in the lifted protocol."]`. Running it on 2.3.0 panics in `MerkleProverLifted::decommit`.

The last release that works is **v1.0.0**. That forces three things on this spike:
- git dependencies pinned to tag `v1.0.0`, because `stwo-constraint-framework` 1.0.0 is not on crates.io;
- the `nightly-2025-04-06` toolchain;
- downgrades of `enum-ordinalize` and `enum-ordinalize-derive` in `Cargo.lock` so they build on that nightly.

[src/poseidon.rs](src/poseidon.rs) is vendored from the v1.0.0 example (Apache-2.0) with a single change: one permutation per row instead of eight, so the trace has about the same width as the Plonky3 spike (~158 columns, against Plonky3's 164).

## Results (128 permutations, 16-core desktop, 2026-10-08)

The security level is Stwo's own score, `pow_bits + log_blowup · n_queries`, which is the optimistic ethSTARK conjecture. Grinding is 16 bits. Folding is binary, because v1.0.0 has no arity setting.

| layout | blowup | queries | proof KB | prove ms | verify ms |
|---|---:|---:|---:|---:|---:|
| 8 perms/row (upstream, ~1,264 columns) | 16 | 28 | 195.0 | 63.8 | 1.48 |
| 8 perms/row | 2 | 112 | 227.7 | 46.9 | 1.59 |
| **1 perm/row (~158 columns)** | **16** | **28** | **71.4** | **49.7** | **1.13** |
| 1 perm/row | 8 | 38 | 73.9 | 59.7 | 1.10 |
| 1 perm/row | 4 | 56 | 84.8 | 37.0 | 1.31 |

## Verdict: Stwo rejected for the Shielded Space

1. **Release state.** Only a superseded release on an old nightly can prove our AIR class. We would be pinning consensus-critical verifier code to an abandoned branch.
2. **Field size.** The challenge field is fixed at QM31, 124 bits. The Plonky3 analysis (`p3-security`, the same terms apply to Circle STARKs) shows that a 124-bit field caps soundness at about 105–108 bits for this statement. Stwo's score doesn't model that ceiling, so its "128 bits" can't be reached for us under either the proven or the stricter conjectured notion.
3. **Size and speed.** At the same width and its own *optimistic* score, Stwo's proof is ~71 KB. Plonky3's is ~55 KB under a *stricter* conjectured formula, or ~95 KB proven. Proving is also 4–8× slower at this small trace size; Stwo is optimised for much larger traces.
