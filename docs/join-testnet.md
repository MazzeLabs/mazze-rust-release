# Join the Mazze testnet

This guide takes a fresh machine to a running Mazze node on the live testnet,
and optionally to mining. Everything runs from this repository; the default
configuration in `run/hydra.toml` already points at the testnet.

| | |
|---|---|
| Native chain id | `1990` (addresses look like `mazze:aa…`) |
| eSpace (EVM) chain id | `1991` |
| Genesis block | `0x5c7ddc51abd931dabe28461e1887ededfd38c4da9ec4dbae5dd03b63cb414355` |
| Public RPC (native) | `http://37.27.134.36:52537`, `http://37.27.134.37:52537` |
| Public RPC (eSpace / MetaMask) | `http://37.27.134.36:58545` |
| P2P port | `55555` TCP and UDP |

**Short on time, RAM or disk?** Build (sections 2–3), then go straight to
[Fast sync](#4-pick-how-to-sync): it reaches the tip in under an hour with
about 25 GB of disk. Replaying the whole chain from genesis takes days on a
home machine.

## 1. What you need

| | Full node (fast sync) | Archive node | Add for mining |
|---|---|---|---|
| CPU | 4 cores | 4 cores | as many as you can spare |
| RAM | 16 GB (8 GB in low-memory mode, see below) | 16 GB | 0.3 GB, or 2.5 GB for full speed |
| Disk (SSD) | ~25 GB, 50 GB free recommended | 30–70 GB and growing, 100 GB free recommended | – |
| Time to the tip | under an hour | days | – |

Supported systems:

- **Linux x86_64**: Ubuntu 22.04 and 24.04 are what we test on.
- **Windows**: through WSL2 (Ubuntu). There is no native Windows build of the
  node or the miner. See [Windows (WSL2)](#windows-wsl2).
- **macOS**: should build and run, but is not tested yet; reports welcome.
  See [macOS](#macos).

Port `55555` TCP and UDP reachable from the internet lets other nodes connect
to you. Behind NAT the node still works, it just won't accept inbound peers.

## 2. Install the build dependencies

### Linux (and WSL2)

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config clang cmake llvm-dev \
  libclang-dev libssl-dev liblz4-dev zlib1g-dev libzstd-dev libsnappy-dev \
  libbz2-dev libsqlite3-dev libhwloc-dev git curl python3

# Rust. The exact toolchain is pinned by rust-toolchain.toml and installed
# automatically by the first cargo command.
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
```

### Windows (WSL2)

1. In PowerShell as administrator: `wsl --install -d Ubuntu-24.04`, then
   reboot and open Ubuntu from the Start menu.
2. WSL gets only half of your Windows RAM by default, which is not enough for
   the node. Create `C:\Users\<you>\.wslconfig`:

   ```ini
   [wsl2]
   memory=12GB
   swap=8GB
   ```

   then run `wsl --shutdown` in PowerShell and open Ubuntu again. With 16 GB
   of Windows RAM or less, also use the [low-memory mode](#low-memory-mode).
3. Clone and build **inside** the Linux file system (`~/`), not under
   `/mnt/c/`: the node is many times slower on Windows drives.
4. Follow the Linux steps above.

WSL's virtual disk grows but does not shrink when you delete files. To get the
space back on Windows: `wsl --shutdown`, then compact the `ext4.vhdx` with
`Optimize-VHD` (Hyper-V) or `diskpart` → `select vdisk file=…` →
`compact vdisk`.

### macOS

Not tested yet. With [Homebrew](https://brew.sh):

```bash
xcode-select --install
brew install cmake pkg-config llvm openssl hwloc lz4 zstd snappy python3
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
```

On Apple Silicon (M1–M4) RandomX runs through its ARM64 compiler, which is the
part most likely to misbehave; if hashing fails or is very slow, please open
an issue. macOS may ask to allow incoming connections on the first start.

## 3. Build

```bash
git clone https://github.com/MazzeLabs/mazze-rust-release
cd mazze-rust-release
cargo build --release --bin mazze --bin mazze-miner
```

The first build takes 10–30 minutes and the binaries end up in
`target/release/`. The build directory takes several GB; if you ever build
without `--release`, `target/debug/` can grow to tens of GB and is safe to
delete.

## 4. Pick how to sync

### Fast sync (recommended)

A **full** node skips replaying history: it starts from two recent era
checkpoints, downloads the state at a checkpoint as a verified snapshot, and
checks the proof of work of every header after it. It keeps only recent
history (from about the last era checkpoint up).

The trade-off: you trust the checkpoint hashes you put in the config. The
helper below reads them from two independent archive nodes and refuses to
continue if they disagree. Compare them with another source you trust before
using them.

```bash
./run/fast-sync-anchors.sh
# prints: trusted_checkpoints = "C:0x…:N,C-20000:0x…:N,C-40000:0x…:N"
```

Then edit `run/hydra.toml`:

```toml
node_type = "full"
trusted_checkpoints = "…the line printed above…"
```

If the node already ran before, stop it and delete its chain data first
(this keeps your node key):

```bash
./run/stop.sh
rm -rf blockchain_data/blockchain_db blockchain_data/storage_db
```

### Archive node (default)

Leave `run/hydra.toml` as it is. The node downloads the whole chain from
genesis and verifies and executes every block itself, so it trusts nobody.
It is what you want for old blocks, receipts and state, and it takes days.

### Low-memory mode

To verify proof of work the node keeps up to three RandomX datasets of ~2 GB
each in memory. On a machine with 16 GB or less (or WSL), set in
`run/hydra.toml`:

```toml
randomx_full_datasets = 1   # ~4 GB less
# or
randomx_full_datasets = 0   # ~6 GB less; verification several times slower
```

`0` is enough to follow the tip after a fast sync, but makes catching up
slow. Don't run the miner with `RANDOMX_FULL_MEM=1` on such a machine.

## 5. Start the node

```bash
./run/start-node.sh
```

The node creates its data directory (`blockchain_data/`), connects to the
testnet bootnodes listed in `run/hydra.toml` and starts syncing.

### Check that it's syncing

```bash
# The node's own view (local RPC, all APIs):
curl -s -X POST -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"mazze_getStatus","params":[]}' \
  http://127.0.0.1:12539

# The network's tip, for comparison:
curl -s -X POST -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"mazze_epochNumber","params":[]}' \
  http://37.27.134.36:52537

# Logs:
tail -F run/logs/mazze-node.log | grep -a "phase\|epoch"
```

While syncing, the log shows phases such as `CatchUpSyncBlockHeaderPhase` and
`CatchUpFillBlockBodyPhase`; a fast sync also logs
`fast-sync anchor: rooting at checkpoint …` and the snapshot restore. The node
is caught up when it logs `start phase "NormalSyncPhase"` and its
`epochNumber` matches the network's.

### Logs

- `run/logs/mazze-node.log`: the node's log. It rolls over at 200 MB and keeps
  five compressed archives in `run/logs/archive/`, so it stays under ~300 MB.
- `run/logs/mazze-node.console.log`: what bypasses the log, such as startup
  errors and crash backtraces. Look here if the node stops by itself.
- `run/logs/mazze-miner.log`: the miner.

### Stop, restart, upgrade

```bash
./run/stop.sh                 # stop the node (and the miner, if running)
./run/start-node.sh           # start again; it resumes from its database

# Upgrade to the latest code:
./run/stop.sh
git pull
cargo build --release --bin mazze --bin mazze-miner
./run/start-node.sh
```

## 6. Create a wallet

The CLI talks to your node's local RPC.

```bash
./run/mazze-cli.sh wallet new alice
./run/mazze-cli.sh wallet address --name alice     # your mazze:aa… address
./run/mazze-cli.sh wallet balance --name alice
```

Wallets are stored in `~/.mazze`. See [docs/mazze-cli.md](mazze-cli.md) for
transfers and the other commands.

## 7. Mine (optional)

Mining needs a node that is caught up (NormalSyncPhase).

1. Put your address in `run/hydra.toml`:

   ```toml
   mining_author = "mazze:aa…your address…"
   ```

2. Restart the node so it picks up the new address (the node reads it at
   startup; restarting only the miner is not enough):

   ```bash
   ./run/stop.sh && ./run/start-node.sh
   ```

3. Start the miner once the node is back at the tip:

   ```bash
   ./run/start-miner.sh
   tail -F run/logs/mazze-miner.log
   ```

The miner connects to your own node over Stratum on `127.0.0.1:32525`; the
shared secret is generated on first start in `run/hydra.secrets.toml`. It uses
all cores but two and runs at the lowest priority so the node always keeps up.

- `NUM_THREADS=<n> ./run/start-miner.sh` chooses the thread count.
- By default the miner uses RandomX light mode (~256 MB). With 2.5 GB of RAM
  to spare, `RANDOMX_FULL_MEM=1 ./run/start-miner.sh` mines several times
  faster.

Mining is RandomX, so it is CPU work; a GPU does not help. Your node log shows
`Mined block 0x…` for every block you find, and your balance grows as rewards
mature.

## 8. Connect a wallet (eSpace / MetaMask)

eSpace is the EVM side of Mazze. In MetaMask, add a network:

| Field | Value |
|---|---|
| Network name | Mazze Testnet eSpace |
| RPC URL | `http://127.0.0.1:58545` (your node) or `http://37.27.134.36:58545` |
| Chain ID | `1991` |
| Currency symbol | `MAZZE` |

## 9. Ports

| Port | What | Expose? |
|---|---|---|
| 55555 TCP/UDP | P2P | Yes, for inbound peers |
| 52537 | Native JSON-RPC over HTTP (`safe` APIs only) | Optional |
| 52535 | Native JSON-RPC over WebSocket | Optional |
| 58545 / 58546 | eSpace JSON-RPC, HTTP / WebSocket | Optional |
| 12539 | Local RPC with all APIs (used by the CLI) | **No** |
| 32525 | Stratum for your miner (`127.0.0.1` only) | **No** |

## 10. Troubleshooting

- **The node gets killed / stops by itself.** Usually out of memory: check
  `dmesg | grep -i oom` (Linux) and `run/logs/mazze-node.console.log`. Use the
  [low-memory mode](#low-memory-mode), give WSL more memory, or stop the miner
  while syncing.
- **The disk fills up.** The chain data is in `blockchain_data/`; see the
  sizes in section 1. Old installs wrote an unbounded
  `run/logs/mazze-node.log`: delete it while the node is stopped. Also check
  `target/debug/` and, on Windows, compact the WSL disk (section 2).
- **No peers / sync doesn't start.** Check that outbound TCP to port 55555 is
  allowed. `grep -a "peer" run/logs/mazze-node.log | tail` shows connection
  attempts.
- **Every peer disconnects you right after connecting, and the node never
  leaves the first sync phase.** Your `blockchain_data/` was most likely
  created for an older testnet (the testnet was relaunched with a new
  genesis). Stop the node, delete `blockchain_data/blockchain_db` and
  `blockchain_data/storage_db`, and start again.
- **Sync seems stuck.** Compare `mazze_epochNumber` on your node and on the
  public RPC a few minutes apart. If yours does not move, restart the node
  (`./run/stop.sh && ./run/start-node.sh`) and open an issue with the last 200
  lines of `run/logs/mazze-node.log` and of `run/logs/mazze-node.console.log`.

More: [docs/setup-guide.md](setup-guide.md) (Docker and dev mode),
[docs/mining.md](mining.md), [docs/rpc.md](rpc.md),
[docs/viewing-logs.md](viewing-logs.md).
