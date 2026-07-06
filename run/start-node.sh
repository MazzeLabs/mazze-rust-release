#!/usr/bin/env bash
set -euo pipefail

ulimit -n 200000 || true

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
EXECUTABLE="$REPO_ROOT/target/release/mazze"
CONFIG_FILE="$SCRIPT_DIR/hydra.toml"
LOG_DIR="$REPO_ROOT/run/logs"
PID_FILE="$SCRIPT_DIR/node_pid.txt"
LOG_FILE="$LOG_DIR/mazze-node.log"

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

echo "-------$(date '+%Y-%m-%d %H:%M:%S')-------" >> "$LOG_FILE"

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

# Ensure log_conf in config is absolute so it works regardless of CWD
ABS_LOG_CONF="$SCRIPT_DIR/log.yaml"
TEMP_CONF="$SCRIPT_DIR/hydra.runtime.toml"
cp "$CONFIG_FILE" "$TEMP_CONF"
if grep -q '^\s*log_conf\s*=' "$TEMP_CONF"; then
  sed -i "s#^\s*log_conf\s*=.*#log_conf = \"$ABS_LOG_CONF\"#" "$TEMP_CONF"
else
  printf '\nlog_conf = "%s"\n' "$ABS_LOG_CONF" >> "$TEMP_CONF"
fi
# NOTE: We intentionally do NOT inject stratum_secret into the node config.
# The stratum server uses keccak(miner_sent) == node_secret, so node and miner
# cannot share the same literal value. Instead the node runs stratum with no
# secret (auth disabled) bound to 127.0.0.1 (see stratum_listen_address in
# hydra.toml); the co-located miner connects over localhost only. The miner
# still keeps its own stratum_secret entry (start-miner.sh) since the binary
# requires the field to be present.
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
if has_chain_data "$REPO_ROOT/blockchain_data"; then
  if grep -q '^\s*execute_genesis\s*=' "$TEMP_CONF"; then
    sed -i "s#^\s*execute_genesis\s*=.*#execute_genesis = false#" "$TEMP_CONF"
  else
    printf '\nexecute_genesis = false\n' >> "$TEMP_CONF"
  fi
fi

# Storage Phase 3 — enable all 7 MDBX shadow mirrors so every write is
# dual-written to MDBX alongside ParityDB from genesis. Required before reads
# can be cut over to MDBX via debug_mdbxSetReadSource(table, "shadow").
for _f in enable_mdbx_shadow_hash_by_number enable_mdbx_shadow_tx_index \
          enable_mdbx_shadow_blamed_header_verified_roots \
          enable_mdbx_shadow_block_traces enable_mdbx_shadow_blocks \
          enable_mdbx_shadow_epoch_numbers enable_mdbx_shadow_misc; do
  if grep -q "^[[:space:]]*${_f}[[:space:]]*=" "$TEMP_CONF"; then
    sed -i "s#^[[:space:]]*${_f}[[:space:]]*=.*#${_f} = true#" "$TEMP_CONF"
  else
    printf '\n%s = true\n' "$_f" >> "$TEMP_CONF"
  fi
done

# Run from the repository root so log4rs and helper logs share the same
# canonical ./logs directory.
pushd "$REPO_ROOT" >/dev/null

"$EXECUTABLE" --config "$TEMP_CONF" >> "$LOG_FILE" 2>&1 &
PID=$!
echo "$PID" > "$PID_FILE"
echo "Mazze node started (pid=$PID). Logs: $LOG_FILE"
popd >/dev/null
