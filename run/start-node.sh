#!/usr/bin/env bash
set -euo pipefail

ulimit -n 200000 || true

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
EXECUTABLE="$REPO_ROOT/target/release/mazze"
CONFIG_FILE="$SCRIPT_DIR/hydra.toml"
LOG_DIR="$SCRIPT_DIR/logs"
PID_FILE="$SCRIPT_DIR/node_pid.txt"
LOG_FILE="$LOG_DIR/mazze-node.log"

mkdir -p "$LOG_DIR"

if [[ ! -x "$EXECUTABLE" ]]; then
  echo "Error: binary not found at $EXECUTABLE. Did you run: cargo build --release?" >&2
  exit 1
fi

if [[ ! -f "$CONFIG_FILE" ]]; then
  echo "Error: config not found at $CONFIG_FILE" >&2
  exit 1
fi

export MAZZE_SHIELDED_VK_HEX="${MAZZE_SHIELDED_VK_HEX:-$SCRIPT_DIR/shielded_vk.hex}"

echo "-------$(date '+%Y-%m-%d %H:%M:%S')-------" >> "$LOG_FILE"

# Ensure log_conf in config is absolute so it works regardless of CWD
ABS_LOG_CONF="$SCRIPT_DIR/log.yaml"
TEMP_CONF="$SCRIPT_DIR/hydra.runtime.toml"
cp "$CONFIG_FILE" "$TEMP_CONF"
if grep -q '^\s*log_conf\s*=' "$TEMP_CONF"; then
  sed -i "s#^\s*log_conf\s*=.*#log_conf = \"$ABS_LOG_CONF\"#" "$TEMP_CONF"
else
  printf '\nlog_conf = "%s"\n' "$ABS_LOG_CONF" >> "$TEMP_CONF"
fi
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
if has_chain_data "$SCRIPT_DIR/blockchain_data"; then
  if grep -q '^\s*execute_genesis\s*=' "$TEMP_CONF"; then
    sed -i "s#^\s*execute_genesis\s*=.*#execute_genesis = false#" "$TEMP_CONF"
  else
    printf '\nexecute_genesis = false\n' >> "$TEMP_CONF"
  fi
fi

# Run from the config directory so relative paths (blockchain_data, logs, etc.)
# stay consistent across restarts.
pushd "$SCRIPT_DIR" >/dev/null

"$EXECUTABLE" --config "$TEMP_CONF" >> "$LOG_FILE" 2>&1 &
PID=$!
echo "$PID" > "$PID_FILE"
echo "Mazze node started (pid=$PID). Logs: $LOG_FILE"
popd >/dev/null
