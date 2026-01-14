#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

PID_FILES=(
  "$SCRIPT_DIR/node_pid_dev.txt"
  "$SCRIPT_DIR/node_pid.txt"
  "$SCRIPT_DIR/miner_pid.txt"
  "$SCRIPT_DIR/miner_pid_dev.txt"
)

kill_pid() {
  local pid="$1"
  local name="$2"
  local timeout="${3:-5}"
  if ! kill -0 "$pid" >/dev/null 2>&1; then
    echo "No process found with PID: $pid ($name)"
    return 1
  fi
  kill "$pid" >/dev/null 2>&1 || true
  local end=$((SECONDS + timeout))
  while kill -0 "$pid" >/dev/null 2>&1; do
    if (( SECONDS >= end )); then
      kill -9 "$pid" >/dev/null 2>&1 || true
      echo "Killed process with PID: $pid ($name) with SIGKILL"
      return 0
    fi
    sleep 0.2
  done
  echo "Killed process with PID: $pid ($name)"
  return 0
}

found_pid_file=0
for PID_FILE in "${PID_FILES[@]}"; do
  if [[ -f "$PID_FILE" ]]; then
    found_pid_file=1
    PID="$(cat "$PID_FILE" 2>/dev/null || true)"
    if [[ -n "$PID" ]]; then
      kill_pid "$PID" "$PID_FILE" || true
    else
      echo "Empty PID file: $PID_FILE"
    fi
    rm -f "$PID_FILE"
    echo "Removed $PID_FILE"
  fi
done

if [[ "$found_pid_file" -eq 0 ]]; then
  echo "No PID files found; trying to locate running mazze processes..."
  mapfile -t pids < <(pgrep -f "mazze.*--config .*hydra.*\\.toml" || true)
  if [[ ${#pids[@]} -eq 0 ]]; then
    mapfile -t pids < <(pgrep -f "target/(debug|release)/mazze" || true)
  fi
  if [[ ${#pids[@]} -eq 0 ]]; then
    echo "No mazze processes found."
  else
    for pid in "${pids[@]}"; do
      kill_pid "$pid" "pgrep" || true
    done
  fi
fi

echo "Stop process completed."
