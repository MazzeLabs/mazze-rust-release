#!/usr/bin/env bash
set -euo pipefail

ulimit -n 200000 || true

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
EXECUTABLE="$REPO_ROOT/target/debug/mazze"
CONFIG_FILE="$SCRIPT_DIR/hydra.toml"
LOG_DIR="$SCRIPT_DIR/logs"
PID_FILE="$SCRIPT_DIR/node_pid_dev.txt"
LOG_FILE="$LOG_DIR/mazze-node-dev.log"
TEMP_CONF="$SCRIPT_DIR/hydra.dev.runtime.toml"
ABS_LOG_CONF="$SCRIPT_DIR/log.yaml"
ABS_GENESIS_SECRETS="$REPO_ROOT/bins/mazze/genesis_secrets.toml"
DATA_DIR="$SCRIPT_DIR/blockchain_data_dev"

mkdir -p "$LOG_DIR" "$DATA_DIR"

if [[ ! -x "$EXECUTABLE" ]]; then
  echo "Debug binary not found at $EXECUTABLE. Building..." >&2
  cargo build -p mazze
fi

if [[ ! -f "$CONFIG_FILE" ]]; then
  echo "Error: config not found at $CONFIG_FILE" >&2
  exit 1
fi

export MAZZE_SHIELDED_VK_HEX="${MAZZE_SHIELDED_VK_HEX:-$SCRIPT_DIR/shielded_vk.hex}"

if [[ ! -f "$SCRIPT_DIR/shielded_vk.hex" || ! -f "$SCRIPT_DIR/shielded_pk.hex" ]]; then
  echo "Warning: shielded key files not found; shielded proofs will fail until you generate them." >&2
  echo "Hint: cargo run -p mazze-executor --bin shielded_keygen -- --out run/shielded_vk.hex --out-pk run/shielded_pk.hex" >&2
fi

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

set_kv "bootnodes" "\"\""
set_kv "node_type" "\"full\""
set_kv "public_address" "\"127.0.0.1\""
set_kv "mode" "\"dev\""
set_kv "dev_block_interval_ms" "500"
set_kv "log_level" "\"debug\""
set_kv "log_conf" "\"$ABS_LOG_CONF\""
set_kv "mining_type" "\"disable\""
set_kv "jsonrpc_local_http_port" "12539"
set_kv "genesis_secrets" "\"$ABS_GENESIS_SECRETS\""
set_kv "mazze_data_dir" "\"$DATA_DIR\""
if compgen -G "$DATA_DIR/blockchain_db/*" > /dev/null; then
  set_kv "execute_genesis" "false"
else
  set_kv "execute_genesis" "true"
fi

echo "-------$(date '+%Y-%m-%d %H:%M:%S')-------" >> "$LOG_FILE"

pushd "$SCRIPT_DIR" >/dev/null
"$EXECUTABLE" --config "$TEMP_CONF" >> "$LOG_FILE" 2>&1 &
PID=$!
echo "$PID" > "$PID_FILE"
echo "Mazze dev node started (pid=$PID). Logs: $LOG_FILE"
popd >/dev/null
