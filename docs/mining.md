# Mazze Mining Guide

Mazze uses Proof-of-Work backed by RandomX. A node validates PoW and assembles
blocks; miners search nonces that satisfy the current difficulty target.

## Mining modes (node config)
Mining behavior is controlled by `run/hydra.toml`:
- `mining_author`: reward address (base32 or 40-hex). Required for mining.
- `mining_type`: `stratum`, `cpu`, or `disable`.
- `stratum_listen_address`, `stratum_port`: stratum endpoint for external miners.
- `stratum_secret`: 64-hex secret used to authorize stratum workers (required by
  `mazze-miner`).
- `pow_problem_window_size`: PoW manager window size.

If `mining_type` is not set, the node defaults to:
- `stratum` when `mining_author` and `stratum_secret` are set.
- `cpu` when `mining_author` is set but `stratum_secret` is not.
- `disable` otherwise.

## Stratum mining (recommended for production)
1. Set your reward address:
   - `mining_author = "<base32-or-hex>"`
2. Enable stratum:
   - `mining_type = "stratum"`
   - `stratum_listen_address = "0.0.0.0"` (or bind to a specific IP)
   - `stratum_port = 32525`
   - `stratum_secret = "<64-hex>"` (required by `mazze-miner`)
3. Start the node:
```bash
cargo build --release -p mazze
./run/start-node.sh
```
4. Start the miner (local):
```bash
cargo build --release -p mazze-miner
./run/start-miner.sh
```

### Miner configuration
`mazze-miner` reads the stratum address and secret from the node config.
You can override settings on the command line:
```bash
./target/release/mazze-miner \
  --config run/hydra.toml \
  --stratum-address 127.0.0.1:32525 \
  --num-threads 16 \
  --worker-id 1
```

The helper script `run/start-miner.sh` also supports environment variables:
- `NUM_THREADS` (default: 16)
- `WORKER_ID` (default: 1)
- `RANDOMX_FULL_MEM` (default: 0, set to `1` for full-memory RandomX)

If miners are remote, expose `stratum_port` and set
`stratum_listen_address` accordingly.

## CPU mining (single-node)
For a single machine without stratum workers:
1. Set `mining_author`.
2. Set `mining_type = "cpu"`.
3. Start the node (`./run/start-node.sh`).

No separate miner process is needed in CPU mode.

## Dev mode (no PoW)
In dev mode, blocks can be generated without PoW:
- `mode = "dev"`
- `dev_block_interval_ms = 500` (or omit for tx-triggered blocks)
- `mining_type = "disable"`

Use `run/start-node-dev.sh` to generate a dev config automatically.

## Monitoring mining
- Logs: `run/logs/mazze-node.log` and `run/logs/mazze-miner.log`.
- RPC checks:
  - `mazze_getStatus` (chain progress).
  - `mazze_getBlockRewardInfo` (reward data per epoch).

## Related docs
- `docs/architecture/pow-and-mining.md`
- `docs/setup-guide.md`
- `docs/viewing-logs.md`
