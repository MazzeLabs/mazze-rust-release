#!/usr/bin/env bash
set -euo pipefail

ulimit -n 200000 || true

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
EXECUTABLE="$REPO_ROOT/target/release/mazze"
BASE_CONF="$SCRIPT_DIR/hydra.toml"
TEMP_CONF="$SCRIPT_DIR/hydra.soak.runtime.toml"
LOG_DIR="$SCRIPT_DIR/logs"
LOG_FILE="$LOG_DIR/mazze-soak.log"
METRICS_FILE="$LOG_DIR/metrics-soak.log"
PID_FILE="$SCRIPT_DIR/soak_pid.txt"

mkdir -p "$LOG_DIR"

if [[ ! -x "$EXECUTABLE" ]]; then
  echo "Error: binary not found at $EXECUTABLE. Did you run: cargo build --release?" >&2
  exit 1
fi

if [[ ! -f "$BASE_CONF" ]]; then
  echo "Error: config not found at $BASE_CONF" >&2
  exit 1
fi

TARGET_BPS="${TARGET_BPS:-4}"
BLOCK_INTERVAL_MS="${BLOCK_INTERVAL_MS:-}"
TX_PERIOD_US="${TX_PERIOD_US:-1000}"
TXGEN_ACCOUNT_COUNT="${TXGEN_ACCOUNT_COUNT:-100}"
MDBX_MAP_SIZE_MB="${MDBX_MAP_SIZE_MB:-65536}"
METRICS_INTERVAL_MS="${METRICS_INTERVAL_MS:-2000}"
MODE="${MODE:-dev}"
NODE_TYPE="${NODE_TYPE:-full}"
LOG_LEVEL="${LOG_LEVEL:-info}"
GENERATE_TX="${GENERATE_TX:-true}"
BOOTNODES="${BOOTNODES:-}"

if [[ -z "$BLOCK_INTERVAL_MS" ]]; then
  if ! [[ "$TARGET_BPS" =~ ^[0-9]+$ ]] || [[ "$TARGET_BPS" -le 0 ]]; then
    echo "Error: TARGET_BPS must be a positive integer." >&2
    exit 1
  fi
  BLOCK_INTERVAL_MS=$((1000 / TARGET_BPS))
fi

if [[ "$BLOCK_INTERVAL_MS" -lt 1 ]]; then
  BLOCK_INTERVAL_MS=1
fi

set_conf() {
  local key="$1"
  local value="$2"
  if grep -q "^[[:space:]]*$key[[:space:]]*=" "$TEMP_CONF"; then
    sed -i "s#^[[:space:]]*$key[[:space:]]*=.*#$key = $value#" "$TEMP_CONF"
  else
    printf '\n%s = %s\n' "$key" "$value" >> "$TEMP_CONF"
  fi
}

cp "$BASE_CONF" "$TEMP_CONF"

ABS_LOG_CONF="$SCRIPT_DIR/log.yaml"
set_conf "log_conf" "\"$ABS_LOG_CONF\""
set_conf "log_level" "\"$LOG_LEVEL\""
set_conf "bootnodes" "\"$BOOTNODES\""
set_conf "mode" "\"$MODE\""
set_conf "node_type" "\"$NODE_TYPE\""
set_conf "catch_up_mode" "false"
set_conf "dev_block_interval_ms" "$BLOCK_INTERVAL_MS"
set_conf "mining_type" "\"disable\""
set_conf "generate_tx" "$GENERATE_TX"
set_conf "generate_tx_period_us" "$TX_PERIOD_US"
set_conf "txgen_account_count" "$TXGEN_ACCOUNT_COUNT"
set_conf "metrics_enabled" "true"
set_conf "metrics_output_file" "\"$METRICS_FILE\""
set_conf "metrics_report_interval_ms" "$METRICS_INTERVAL_MS"
set_conf "mdbx_map_size_mb" "$MDBX_MAP_SIZE_MB"

if [[ -n "${BLOCK_CACHE_GC_MS:-}" ]]; then
  set_conf "block_cache_gc_period_ms" "$BLOCK_CACHE_GC_MS"
fi
if [[ -n "${STORAGE_MAX_OPEN_SNAPSHOTS:-}" ]]; then
  set_conf "storage_max_open_snapshots" "$STORAGE_MAX_OPEN_SNAPSHOTS"
fi
if [[ -n "${STORAGE_MAX_OPEN_MPT_COUNT:-}" ]]; then
  set_conf "storage_max_open_mpt_count" "$STORAGE_MAX_OPEN_MPT_COUNT"
fi
if [[ -n "${STORAGE_DELTA_MPTS_CACHE_SIZE:-}" ]]; then
  set_conf "storage_delta_mpts_cache_size" "$STORAGE_DELTA_MPTS_CACHE_SIZE"
fi
if [[ -n "${STORAGE_DELTA_MPTS_CACHE_START_SIZE:-}" ]]; then
  set_conf "storage_delta_mpts_cache_start_size" "$STORAGE_DELTA_MPTS_CACHE_START_SIZE"
fi
if [[ -n "${STORAGE_DELTA_MPTS_SLAB_IDLE_SIZE:-}" ]]; then
  set_conf "storage_delta_mpts_slab_idle_size" "$STORAGE_DELTA_MPTS_SLAB_IDLE_SIZE"
fi

printf '-------%s-------\n' "$(date '+%Y-%m-%d %H:%M:%S')" >> "$LOG_FILE"

pushd "$SCRIPT_DIR" >/dev/null
"$EXECUTABLE" --config "$TEMP_CONF" >> "$LOG_FILE" 2>&1 &
PID=$!
echo "$PID" > "$PID_FILE"
echo "Mazze soak node started (pid=$PID). Logs: $LOG_FILE"
echo "Metrics: $METRICS_FILE"
popd >/dev/null
