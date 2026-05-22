#!/usr/bin/env bash
# Start a Mazze node tuned as an "archive" RPC server (explorer-facing).
#
# Profile rationale:
# - node_type = "archive" keeps all historical state/receipts/traces/tx index.
# - persist_block_number_index + persist_tx_index = true so the explorer can
#   look up blocks by number and txs by hash.
# - Bigger ledger_cache_size because eth_getLogs scans replay a lot of data.
# - Fewer outgoing peers than a miner — bandwidth goes to serving RPC.
# - get_logs_* limits prevent a single explorer query from saturating the node.
# - mining_type = "disable" so an archive node never mines.
set -euo pipefail

ulimit -n 200000 || true

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
EXECUTABLE="$REPO_ROOT/target/release/mazze"
CONFIG_FILE="$SCRIPT_DIR/hydra.toml"
LOG_DIR="$REPO_ROOT/run/logs"
PID_FILE="$SCRIPT_DIR/node_pid_archive.txt"
LOG_FILE="$LOG_DIR/mazze-node-archive.log"
TEMP_CONF="$SCRIPT_DIR/hydra.archive.runtime.toml"
ABS_LOG_CONF="$SCRIPT_DIR/log.yaml"

mkdir -p "$LOG_DIR" "$LOG_DIR/archive"

if [[ ! -x "$EXECUTABLE" ]]; then
  echo "Error: binary not found at $EXECUTABLE. Did you run: cargo build --release?" >&2
  exit 1
fi

if [[ ! -f "$CONFIG_FILE" ]]; then
  echo "Error: config not found at $CONFIG_FILE" >&2
  exit 1
fi

export MAZZE_SHIELDED_VK_HEX="${MAZZE_SHIELDED_VK_HEX:-$SCRIPT_DIR/shielded_vk.hex}"

cp "$CONFIG_FILE" "$TEMP_CONF"

set_kv() {
  local key="$1"
  local value="$2"
  if grep -q "^[[:space:]]*${key}[[:space:]]*=" "$TEMP_CONF"; then
    sed -i "s#^[[:space:]]*${key}[[:space:]]*=.*#${key} = ${value}#" "$TEMP_CONF"
  else
    printf '\n%s = %s\n' "$key" "$value" >> "$TEMP_CONF"
  fi
}

comment_kv() {
  local key="$1"
  sed -i "s#^[[:space:]]*${key}[[:space:]]*=#\# ${key} =#" "$TEMP_CONF"
}

# --- Archive profile -------------------------------------------------------
set_kv "node_type"                            "\"archive\""
set_kv "log_conf"                             "\"$ABS_LOG_CONF\""

# RPC-side: explorer needs to resolve blocks by number and txs by hash.
set_kv "persist_block_number_index"           "true"
set_kv "persist_tx_index"                     "true"

# Bigger cache so log scans don't constantly hit disk.
set_kv "ledger_cache_size"                    "8192"

# Don't burn bandwidth on extra peers — archive nodes don't need many.
set_kv "max_outgoing_peers"                   "12"

# RPC concurrency. Keep moderate — 4 threads is usually plenty per node.
set_kv "jsonrpc_http_threads"                 "4"

# Bound explorer queries so one bad request can't lock the consensus graph.
set_kv "get_logs_filter_max_limit"            "5000"
set_kv "get_logs_epoch_batch_size"            "32"
set_kv "get_logs_filter_max_epoch_range"      "1000"
set_kv "get_logs_filter_max_block_number_range" "1000"

# State DB: mdbx is the modern backend for archive workloads.
set_kv "state_db_type"                        "\"mdbx\""
set_kv "mdbx_map_size_mb"                     "65536"
set_kv "mdbx_max_readers"                     "2048"
set_kv "mdbx_sync_mode"                       "\"relaxed\""

# Mining is disabled on archive nodes regardless of mining_author in hydra.toml.
set_kv "mining_type"                          "\"disable\""
comment_kv "mining_author"

has_chain_data() {
  local base="$1"
  compgen -G "$base/blockchain_db/*" > /dev/null && return 0
  compgen -G "$base/storage_db/*"    > /dev/null && return 0
  compgen -G "$base/net_config/*"    > /dev/null && return 0
  return 1
}
if has_chain_data "$REPO_ROOT/blockchain_data"; then
  set_kv "execute_genesis" "false"
fi

echo "-------$(date '+%Y-%m-%d %H:%M:%S') [archive]-------" >> "$LOG_FILE"

pushd "$REPO_ROOT" >/dev/null
"$EXECUTABLE" --config "$TEMP_CONF" >> "$LOG_FILE" 2>&1 &
PID=$!
echo "$PID" > "$PID_FILE"
echo "Mazze archive node started (pid=$PID). Config: $TEMP_CONF  Logs: $LOG_FILE"
popd >/dev/null
