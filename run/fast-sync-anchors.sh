#!/usr/bin/env bash
# fast-sync-anchors.sh — print the `trusted_checkpoints` line a fresh full node
# needs to join the testnet by checkpoint fast-sync (docs/join-testnet.md).
#
# It reads the latest era checkpoint C from the network and the hash and block
# number of C, C-20000 and C-40000 from two archive nodes, and stops if the two
# disagree. You still trust these archives for the anchors, so check the
# hashes against another source you trust (a block explorer, another operator)
# before using them.
#
# Usage: ./run/fast-sync-anchors.sh [rpc_url_1] [rpc_url_2]
set -euo pipefail

RPC1="${1:-http://37.27.134.36:52537}"
RPC2="${2:-http://37.27.134.37:52537}"
ERA=20000

call() { # url method params-json
  curl -sf --max-time 15 -X POST -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":$3}" "$1"
}

field() { # json field -> value (hex numbers converted to decimal)
  python3 -c '
import json, sys
v = json.load(sys.stdin)["result"][sys.argv[1]]
print(int(v, 16) if isinstance(v, str) and v.startswith("0x") and len(v) < 20 else v)
' "$1"
}

anchors() { # url checkpoint -> "C:hash:bn,C-ERA:hash:bn,C-2ERA:hash:bn"
  local out=() h
  for h in "$2" $(( $2 - ERA )) $(( $2 - 2 * ERA )); do
    local block hash bn
    block=$(call "$1" mazze_getBlockByEpochNumber "[\"$(printf '0x%x' "$h")\",false]")
    hash=$(field hash <<<"$block")
    bn=$(field blockNumber <<<"$block")
    out+=("$h:$hash:$bn")
  done
  (IFS=,; echo "${out[*]}")
}

C=$(call "$RPC1" mazze_getStatus '[]' | field latestCheckpoint)
if [ "$C" -lt $(( 2 * ERA )) ]; then
  echo "The chain has no third era checkpoint yet (latest $C); sync from genesis instead." >&2
  exit 1
fi

A1=$(anchors "$RPC1" "$C")
A2=$(anchors "$RPC2" "$C")
if [ "$A1" != "$A2" ]; then
  echo "The two archives disagree; do not use these anchors:" >&2
  echo "  $RPC1: $A1" >&2
  echo "  $RPC2: $A2" >&2
  exit 1
fi

echo "trusted_checkpoints = \"$A1\""
