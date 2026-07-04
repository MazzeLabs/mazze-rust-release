#!/usr/bin/env bash
set -euo pipefail

ulimit -n 200000 || true

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
EXECUTABLE="$REPO_ROOT/target/debug/mazze"
CONFIG_FILE="$SCRIPT_DIR/hydra.toml"
LOG_DIR="$REPO_ROOT/run/logs"
PID_FILE="$SCRIPT_DIR/node_pid_dev.txt"
LOG_FILE="$LOG_DIR/mazze-node-dev.log"
TEMP_CONF="$SCRIPT_DIR/hydra.dev.runtime.toml"
ABS_LOG_CONF="$SCRIPT_DIR/log.yaml"
ABS_GENESIS_SECRETS="$REPO_ROOT/bins/mazze/genesis_secrets.toml"
DATA_DIR="$SCRIPT_DIR/blockchain_data_dev"

mkdir -p "$LOG_DIR" "$LOG_DIR/archive" "$DATA_DIR"

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

SECRETS_FILE="$SCRIPT_DIR/hydra.secrets.toml"
ensure_stratum_secret() {
  if [[ ! -f "$SECRETS_FILE" ]]; then
    echo "Generating $SECRETS_FILE (first-run stratum secret)..." >&2
    local generated
    if command -v openssl >/dev/null 2>&1; then
      generated="$(openssl rand -hex 32)"
    else
      generated="$(head -c 32 /dev/urandom | xxd -p -c 64)"
    fi
    umask 077
    cat > "$SECRETS_FILE" <<EOF
# Auto-generated per-deployment secrets. Do not commit.
# See run/hydra.secrets.toml.example for the format and rotation guidance.
stratum_secret = "$generated"
EOF
    chmod 600 "$SECRETS_FILE"
  fi
  STRATUM_SECRET="$(grep -E '^[[:space:]]*stratum_secret[[:space:]]*=' "$SECRETS_FILE" \
    | head -n1 \
    | sed -E 's/^[[:space:]]*stratum_secret[[:space:]]*=[[:space:]]*"([^"]*)".*/\1/')"
  if [[ -z "$STRATUM_SECRET" ]]; then
    echo "Error: stratum_secret missing or unparseable in $SECRETS_FILE" >&2
    exit 1
  fi
}
ensure_stratum_secret
set_kv "stratum_secret" "\"$STRATUM_SECRET\""

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

# Storage Phase 2 — turn on every shadow mirror the migration ships
# with, so a local dev node exercises the same write path the fleet
# will after Phase 3 flips reads over. Adjust to `false` if you're
# reproducing an issue that pre-dates a specific flag.
set_kv "enable_mdbx_shadow_hash_by_number" "true"
set_kv "enable_mdbx_shadow_tx_index" "true"
set_kv "enable_mdbx_shadow_blamed_header_verified_roots" "true"
set_kv "enable_mdbx_shadow_block_traces" "true"
set_kv "enable_mdbx_shadow_blocks" "true"
set_kv "enable_mdbx_shadow_epoch_numbers" "true"
set_kv "enable_mdbx_shadow_misc" "true"
has_chain_data() {
  local base="$1"
  if compgen -G "$base/blockchain_db/*" > /dev/null; then
    return 0
  fi
  if compgen -G "$base/storage_db/*" > /dev/null; then
    return 0
  fi
  if compgen -G "$base/net_config/*" > /dev/null; then
    return 0
  fi
  return 1
}
if has_chain_data "$DATA_DIR"; then
  set_kv "execute_genesis" "false"
else
  set_kv "execute_genesis" "true"
fi

echo "-------$(date '+%Y-%m-%d %H:%M:%S')-------" >> "$LOG_FILE"

pushd "$REPO_ROOT" >/dev/null
"$EXECUTABLE" --config "$TEMP_CONF" >> "$LOG_FILE" 2>&1 &
PID=$!
echo "$PID" > "$PID_FILE"
echo "Mazze dev node started (pid=$PID). Logs: $LOG_FILE"
popd >/dev/null
