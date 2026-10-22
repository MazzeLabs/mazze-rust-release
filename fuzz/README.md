# Fuzz targets

Coverage-guided fuzzing (libFuzzer) of the decoders that take untrusted
shielded data. Not part of the node's workspace.

| Target | What it checks |
|---|---|
| `shielded_body` | body decoding never panics; what decodes has exactly one encoding and passes the structural rules |
| `block` | network and stored block decoding never panic; a decoded block re-encodes to a fixed point |
| `proof_wire` | proof decoding never panics nor reads past the cap; one proof, one encoding |
| `verify_body` | stage A never panics on any body and proof; a proof that verifies carries its own body and its canonical encoding |

```bash
cargo install cargo-fuzz
# Real proofs as seeds, so mutations reach the verifier's deep checks:
MAZZE_FUZZ_SEEDS=$PWD/fuzz/corpus cargo test --release -p mazze-shielded-pq \
  --test verify_body write_fuzz_seeds -- --ignored
cd fuzz
cargo +nightly fuzz run verify_body -- -max_len=430000 -rss_limit_mb=4096 -max_total_time=600
```

Run heavy fuzzing inside a memory-capped scope
(`systemd-run --user --scope -p MemoryMax=6G ...`).

Results on 2026-10-22 (no crash in any target):

| Target | Runs | Coverage |
|---|---|---|
| `shielded_body` | 104 M | 235 |
| `block` | 44 M | 724 |
| `proof_wire` | 20.6 M random, 3.6 M from real proofs | 1,086 |
| `verify_body` | 9.8 M random, 32 k from real proofs | 6,284 |
