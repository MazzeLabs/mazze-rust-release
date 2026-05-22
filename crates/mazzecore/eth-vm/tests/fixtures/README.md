# Ethereum state-test fixtures

This directory holds the [`ethereum/tests`](https://github.com/ethereum/tests)
repository as a git submodule. The repo provides the canonical
`GeneralStateTests` JSON corpus used by every mainline Ethereum client
(geth, reth, besu, nethermind, …) to validate EVM correctness across
hard forks.

The harness in
[`tests/state_tests.rs`](../state_tests.rs) reads fixtures from
`./ethereum_tests/GeneralStateTests/` and runs each one against Mazze's
`RevmExec` to confirm semantic parity with mainline Ethereum.

## Setting up

The fixtures are large (several hundred MB) and intentionally not vendored
into this repo. Add them as a submodule:

```bash
# Run from the workspace root.
git submodule add https://github.com/ethereum/tests \
    crates/mazzecore/eth-vm/tests/fixtures/ethereum_tests
git submodule update --init --depth 1

# Or, if cloning fresh:
git clone --recurse-submodules https://github.com/MazzeLabs/mazze-rust-release
```

A `--depth 1` shallow clone is fine — we never need historical revisions
of the test corpus.

## Running

```bash
# By default state tests are #[ignore]'d so plain `cargo test` stays fast.
# Run with --ignored to include them.
cargo test -p mazze-eth-vm --release -- --ignored state_tests
```

`--release` is strongly recommended; debug builds run state tests at
roughly 1–2 fixtures per second, release at 100s/sec.

## Updating the corpus

```bash
cd crates/mazzecore/eth-vm/tests/fixtures/ethereum_tests
git fetch origin
git checkout <newer-tag>
cd ../../../../../..
git add crates/mazzecore/eth-vm/tests/fixtures/ethereum_tests
git commit -m "bump ethereum/tests to <newer-tag>"
```

## Known excluded test groups

The harness skips a few subdirectories that don't apply to Mazze eSpace:

- `Cancun/stEIP4844-blobtransactions/` — Mazze does not implement blob
  transactions (no DA layer); the BLOBHASH opcode returns zero per
  `MazzeDatabase::block_hash`.
- `Prague/stEIP7702/` — EIP-7702 authorization lists are not in scope
  yet (would require executor-layer signature handling).
- Pre-Spurious-Dragon forks — these predate chain-id and many other
  invariants Mazze relies on at the executor layer.

See [`crates/mazzecore/eth-vm/tests/state_tests.rs`](../state_tests.rs) for
the exact exclusion list.
