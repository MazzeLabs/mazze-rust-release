# Mazze CLI

The Mazze CLI (`run/mazze-cli.sh`) is a lightweight wrapper around JSON-RPC for
wallet management, transfers, and shielded pool workflows. You can run it as a
one-shot command or in an interactive shell.

## Prerequisites
- A running node with HTTP RPC enabled.
- For local usage, set `jsonrpc_local_http_port` in `run/hydra.toml`, or use
  `run/start-node-dev.sh` which enables it automatically.
- Alternatively, point the CLI at a remote RPC with `RPC_URL`.
- Rust is required for the CLI to auto-build helper binaries (`mazzekey`,
  `shielded_note`, and the `addrconv` helper for base32/hex conversion).

## Quick start
```bash
./run/mazze-cli.sh status
./run/mazze-cli.sh wallet new alice
./run/mazze-cli.sh wallet balance --name alice
```

Run without arguments to enter interactive mode:
```bash
./run/mazze-cli.sh
mazze> help
```

## Environment variables
- `RPC_URL`: HTTP RPC endpoint (default: `http://127.0.0.1:12539`).
- `MAZZE_NETWORK_ID`: Network id used for address encoding (default: `1990`).
- `MAZZE_DECIMALS`: Token decimals for formatting (default: `18`).
- `MAZZE_HOME`: CLI data directory (default: `~/.mazze`).
- `MAZZE_CLI_DASH`: Enable dashboard in interactive mode (default: `1`).
- `MAZZE_CLI_DASH_REFRESH`: Dashboard refresh interval in seconds (default: `2`).
- `MAZZE_CLI_COLOR`: Force color on/off (default: `1`, or `0` if `NO_COLOR`).
- `MAZZE_SHIELDED_FROM_EPOCH`: Start epoch for shielded note scanning.
- `MAZZE_SHIELDED_LOG_CHUNK`: Log query batch size when scanning shielded notes.
- `MAZZE_FAUCET_AMOUNT`: Default faucet amount in MAZZE for dev/test.
- `MAZZE_WALLET_PASS`: Unlock encrypted wallets non-interactively.

## Commands

### Wallet
- `wallet new <name> [--password]`: Create a new wallet and store the secret.
- `wallet import <name> <secret-hex> [--password]`: Import an existing secret.
- `wallet list [--no-balances]`: List known wallets (optional balance fetch).
- `wallet balance --name <name> [--epoch <epoch>] [--public|--private|--all] [--from-epoch <epoch>]`:
  Show public and/or shielded balances. Shielded scans start at `--from-epoch`.
- `wallet transfer --name <name> --dest <base32|wallet> --amount <value> [--shielded]`:
  Public transfer by default, or shielded transfer when `--shielded` is set.
- `wallet unshield --name <name> --dest <base32|wallet> --amount <value>`:
  Move funds from the shielded pool to a public address.
- `wallet shield-deposit --name <name> --amount <value> [--to <shielded|wallet>]`:
  Deposit public funds into the shielded pool.
- `wallet show <name>`: Print wallet metadata (addresses, encryption, created).
- `wallet address --name <name>`: Print public address.
- `wallet shielded-address --name <name>`: Print shielded address (if present).
- `wallet delete <name>`: Remove wallet files from disk.

### Chain and account queries
- `status`: Raw `mazze_getStatus` response.
- `summary`: Human-friendly status summary.
- `balance <base32|wallet> [epoch]`: Public balance at epoch (default latest).
- `account <base32|wallet> [epoch]`: Account info at epoch.
- `nonce <base32|wallet> [epoch]`: Next nonce.
- `pending <base32|wallet>`: Pending transaction info.
- `pending-txs <base32|wallet> [limit]`: Pending transactions for account.
- `tx <hash>`: Get transaction by hash.
- `receipt <hash>`: Get receipt by hash.
- `tx-status <hash>`: Print receipt or pending status.
- `watch <hash> [interval] [timeout]`: Wait for a receipt to appear.
- `addr <value>`: Convert hex <-> base32 address.
- `dashboard <on|off|status>`: Control the interactive dashboard.
- `wait [seconds]`: Wait for RPC to become responsive.

### Transfers
- `send <from-secret|wallet> <to-base32|wallet> <amount-mazze>`: Send a public transfer.
- `faucet --name <wallet> [--amount <value>]`: Dev/test faucet using genesis accounts.
- `shield-deposit <from-secret|wallet> <amount-mazze> [shielded-output]`:
  Deposit into the shielded pool.
- `shield-send [--inputs <file>] <shielded-output|wallet> <amount-mazze>`:
  Send shielded outputs (requires prebuilt inputs JSON).

### Shielded pool (read-only)
- `root`: Query current shielded pool root.
- `vkhash`: Query the verifying key hash.
- `nullifier <hex32>`: Check a nullifier state.

## Related docs
- `docs/setup-guide.md`
- `docs/shielded-transactions.md`
- `docs/rpc.md`
