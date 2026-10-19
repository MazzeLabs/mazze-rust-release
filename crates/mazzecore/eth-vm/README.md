# mazze-eth-vm

revm-backed EVM for the Mazze eSpace.

Native space continues to run the custom Parity-era interpreter at
[`crates/mazzecore/vm-interpreter`](../vm-interpreter). This crate exists
only for `Space::Ethereum` transactions — the routing happens once, in
[`VmFactory::create`](../executor/src/machine/vm_factory.rs):

```
Space::Ethereum → mazze_eth_vm::RevmExec
Space::Native   → mazze_vm_interpreter::Factory
```

There is no transition flag and no parallel path. eSpace has run on revm
since genesis; the custom interpreter has never seen `Space::Ethereum`
traffic.

## What lives where

| Concern | Location |
|---|---|
| revm `Database` adapter over `vm::Context` | [`src/database.rs`](src/database.rs) |
| `vm::Exec` impl that drives revm `Evm::transact` | [`src/lib.rs`](src/lib.rs) |
| Spec mapping (Mazze `Spec` → revm `SpecId`) | [`src/spec_map.rs`](src/spec_map.rs) |
| State-test harness (Ethereum `GeneralStateTests`) | [`tests/`](tests/) |

## Updating Ethereum fork support

When revm tags a new mainline hard fork:

1. `cargo update -p revm`.
2. Bump [`ESPACE_SPEC`](src/spec_map.rs) to the new `SpecId`.
3. Re-run the state-test harness (`cargo test -p mazze-eth-vm --release
   --tests -- --ignored state_tests`).
4. If any fixture-driven test fails, fix the adapter — don't patch revm.

That is the entire migration recipe. The deliberate design goal is that
future Ethereum hard forks become a dependency bump, not a hand-port.

## Mazze-specific deviations

These are all enforced **outside** revm so revm itself stays unmodified:

- **Cross-space internal contract** — dispatched in `make_executable()`
  *before* `VmFactory::create()`. revm never sees cross-space calls.
- **Sponsor mechanism** — applied in `PreCheckedExecutive::charge_gas`
  pre-VM. revm sees a normalised balance.
- **Storage collateral** — applied in `PreCheckedExecutive::settle_collateral`
  post-VM. We iterate revm's returned state diff to debit `storage_owner`.
- **`evm_gas_ratio = 2`** — applied at the executor layer pre-VM. revm
  receives the post-ratio gas figure.
- **BLOCKHASH limited to the previous block** — encoded in
  [`MazzeDatabase::block_hash`](src/database.rs): any lookback >1 returns
  zero. revm sees a database that simply lacks older hashes.

## State-test harness

`tests/state_tests.rs` runs the upstream Ethereum `GeneralStateTests`
corpus through `RevmExec`. The fixtures live as a git submodule under
`tests/fixtures/ethereum_tests/` (see `tests/fixtures/README.md`).

The harness is `#[ignore]`'d so `cargo test` stays fast for everyday
work. To run it:

```bash
cargo test -p mazze-eth-vm --release --tests -- --ignored state_tests
```

Last clean run: **34036 / 34036 attempted** passing across **2514**
fixture files. (Transaction-acceptance fixtures — intrinsic-gas
underflow, insufficient-funds, fee-cap checks — are excluded because
they validate the tx-acceptance layer, which in Mazze lives in
`PreCheckedExecutive` upstream of `RevmExec`. See
`EXCLUDE_PATH_SUBSTRINGS` in `state_tests.rs`.)
