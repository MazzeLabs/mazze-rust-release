# Mazze Node

The Rust implementation of the Mazze protocol: a proof-of-work blockchain that
orders blocks as a DAG (Tree-Graph), so blocks mined in parallel are all kept
instead of orphaned.

**The testnet is live.** To run a node and mine on it, follow
**[Join the testnet](docs/join-testnet.md)**. It takes a few commands.

## What's in it

- **Tree-Graph consensus.** Blocks reference every tip they see; a pivot chain
  orders them into epochs. The target rate is 4 blocks per second, with ASERT
  difficulty adjustment.
- **RandomX proof of work.** CPU-friendly mining with a built-in Stratum
  server and the `mazze-miner` client.
- **Three spaces on one chain:**
  - **Native space**: Mazze accounts (`mazze:aa…` addresses).
  - **eSpace**: an Ethereum-compatible space that runs on revm, so MetaMask and
    the usual EVM tooling work against it (chain id `1991` on the testnet).
  - **Shielded Space**: private transfers with post-quantum STARK proofs
    (Plonky3, Poseidon2). Amounts and owners stay hidden, and the proofs do not
    rely on elliptic-curve assumptions. Wallet: `mazze-shielded-wallet`.
- **Fast sync.** A new full node starts from trusted era checkpoints and is at
  the tip in minutes; an archive node replays and verifies the whole chain
  from genesis.
- **MDBX storage.** State, blocks and indexes live in MDBX.

## Quick start

```bash
# Dependencies (Ubuntu); see docs/join-testnet.md for details
sudo apt-get install -y build-essential pkg-config clang cmake llvm-dev \
  libclang-dev libssl-dev liblz4-dev zlib1g-dev libzstd-dev libsnappy-dev \
  libbz2-dev libsqlite3-dev libhwloc-dev git curl python3
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

# Build
git clone https://github.com/MazzeLabs/mazze-rust-release
cd mazze-rust-release
cargo build --release --bin mazze --bin mazze-miner

# Run a testnet node (archive by default; see the guide for fast sync,
# which reaches the tip in under an hour, and for a low-memory mode)
./run/start-node.sh
tail -F run/logs/mazze-node.log

# Mine: set mining_author in run/hydra.toml, restart the node, then
./run/start-miner.sh
```

The Rust toolchain version is pinned in `rust-toolchain.toml` and installed
automatically by cargo. Linux is the tested platform; Windows works through
WSL2 and macOS is untested (see the guide).

## Testnet

| | |
|---|---|
| Native chain id | `1990` |
| eSpace chain id | `1991` |
| Public RPC (native) | `http://37.27.134.36:52537` |
| Public RPC (eSpace) | `http://37.27.134.36:58545` |
| P2P | port `55555`; bootnodes are preset in `run/hydra.toml` |

## Repository layout

```
bins/
  mazze/                   the node
  mazze-miner/             RandomX Stratum miner
  mazze-shielded-wallet/   Shielded Space wallet (keys, scanning, proving)
  mazze_key/               key generation (mazzekey)
  mazze_store/             key store management
crates/
  mazzecore/               consensus, sync, execution, storage
  mazzecore/eth-vm/        eSpace on revm
  shielded_pq/             Shielded Space: field, hashes, STARK proof system
  network/                 P2P networking
  stratum/                 Stratum server
  client/                  configuration and JSON-RPC
  …
run/                       start/stop scripts and the testnet config (hydra.toml)
docker/                    node and miner images
docs/                      guides
```

## Documentation

- [Join the testnet](docs/join-testnet.md): build, run, sync, mine, fast sync.
- [Setup guide](docs/setup-guide.md): Docker and local dev mode.
- [Mining](docs/mining.md)
- [CLI wallet](docs/mazze-cli.md)
- [JSON-RPC](docs/rpc.md)
- [Viewing logs](docs/viewing-logs.md)
- [Soak testing](docs/soak-test.md)

## Development

```bash
cargo test -p mazzecore --lib     # core unit tests
./run/start-node-dev.sh           # single-node dev chain
```

Heavy builds and test runs need a lot of RAM; on smaller machines use
`cargo build -j 4` and `cargo test -- --test-threads 2`.

## Security

Please report security issues privately to the Mazze team rather than in a
public issue.

## License

GPL-3.0. See [LICENSE](LICENSE).
