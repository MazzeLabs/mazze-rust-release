#!/usr/bin/env bash
set -euo pipefail

ulimit -n 10000 || true

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
EXECUTABLE="$REPO_ROOT/target/release/mazze-miner"
CONFIG_FILE="$SCRIPT_DIR/hydra.toml"
LOG_DIR="$SCRIPT_DIR/logs"
PID_FILE="$SCRIPT_DIR/miner_pid.txt"
LOG_FILE="$LOG_DIR/mazze-miner.log"

mkdir -p "$LOG_DIR"

if [[ ! -x "$EXECUTABLE" ]]; then
  echo "Error: binary not found at $EXECUTABLE. Did you run: cargo build --release?" >&2
  exit 1
fi

if [[ ! -f "$CONFIG_FILE" ]]; then
  echo "Error: config not found at $CONFIG_FILE" >&2
  exit 1
fi

echo "-------$(date '+%Y-%m-%d %H:%M:%S')-------" >> "$LOG_FILE"

# RANDOMX_FULL_MEM can be exported by the user to control miner memory usage.
pushd "$SCRIPT_DIR" >/dev/null

RANDOMX_FULL_MEM="${RANDOMX_FULL_MEM:-0}" RUST_LOG=info \
"$EXECUTABLE" --config "$CONFIG_FILE" --worker-id "${WORKER_ID:-1}" --num-threads "${NUM_THREADS:-16}" \
  >> "$LOG_FILE" 2>&1 &

PID=$!
echo "$PID" > "$PID_FILE"
echo "Mazze miner started (pid=$PID). Logs: $LOG_FILE"

popd >/dev/null
