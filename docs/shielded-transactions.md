# Shielded Transactions

This guide covers the shielded pool flow, CLI usage, and RPC calls that are
active in the current codebase.

## Prerequisites

- The node must run with a non-zero verifying key hash (VK) on chain.
- If `vkhash` is zero, re-genesis with shielded keys (dev mode):

```bash
./run/regenesis-shielded.sh --dev
./run/mazze-cli.sh vkhash
```

## Shielded addresses

Shielded addresses are base32 encodings of a 64-byte secp256k1 public key.

Generate a shielded address from a secret:

```bash
target/debug/shielded_note address --secret <secret-hex> --network-id 1990
```

## CLI shielded flow

Deposit into the shielded pool:

```bash
./run/mazze-cli.sh wallet shield-deposit --name alice --amount 10
```

Send a shielded transfer:

```bash
./run/mazze-cli.sh wallet transfer --name alice --dest <shielded-address> --amount 5 --shielded
```

Unshield to a public (base32) address:

```bash
./run/mazze-cli.sh wallet unshield --name alice --dest <base32-address> --amount 2
```

Check private balance (scans shielded logs and spent nullifiers):

```bash
./run/mazze-cli.sh wallet balance --name alice --private
```

Notes:
- Proof generation is CPU-heavy in debug builds; use release binaries for faster proving.
- The CLI builds shielded input witnesses automatically.

## Scripts (advanced)

Deposit to the shielded pool:

```
./run/shield-deposit.sh [from-secret-hex] <amount-mazze> [shielded-output]
```

Shielded transfer (requires inputs JSON):

```
./run/send-shielded.sh [shielded-output] <amount-mazze>
```

`run/send-shielded.sh` requires `SHIELDED_INPUTS` (inputs JSON). The CLI sets
this for you when using `wallet transfer --shielded` or `wallet unshield`.

## Shielded note helper

Create a note (commitment + ciphertext):

```bash
target/debug/shielded_note build --to <shielded-address> --value-mazze 10
```

Decrypt a note:

```bash
target/debug/shielded_note decrypt --secret <secret-hex> --commitment <hex> --ciphertext <hex>
```

Build a Poseidon Merkle path:

```bash
target/debug/shielded_note path --commitments <file> --index <n> [--depth 32]
```

## RPC (shielded pool)

Shielded pool address:

```
MAZZE:TYPE.BUILTIN:AAEJUAAAAAAAAAAAAAAAAAAAAAAAAAAABAJ1SJV3W2
```

Selectors:

```
root() -> 0xebf0c717
verifyingKeyHash() -> 0x69b5d6d1
isNullifierSpent(bytes32) -> 0xd5a4e325
```

Example: read the latest root:

```bash
curl -sS -X POST -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"mazze_call","params":[{"to":"MAZZE:TYPE.BUILTIN:AAEJUAAAAAAAAAAAAAAAAAAAAAAAAAAABAJ1SJV3W2","data":"0xebf0c717"},"latest_state"]}' \
  http://127.0.0.1:12539
```

Example: check a nullifier (left-pad to 32 bytes):

```bash
curl -sS -X POST -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"mazze_call","params":[{"to":"MAZZE:TYPE.BUILTIN:AAEJUAAAAAAAAAAAAAAAAAAAAAAAAAAABAJ1SJV3W2","data":"0xd5a4e3250000000000000000000000000000000000000000000000000000000000000000"},"latest_state"]}' \
  http://127.0.0.1:12539
```

Transaction lookup:

```bash
curl -sS -X POST -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"mazze_getTransactionByHash","params":["0x<tx-hash>"]}' \
  http://127.0.0.1:12539
```

Receipt lookup:

```bash
curl -sS -X POST -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"mazze_getTransactionReceipt","params":["0x<tx-hash>"]}' \
  http://127.0.0.1:12539
```
