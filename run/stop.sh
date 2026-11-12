#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Miners first, so they stop submitting to a node that is shutting down.
PID_FILES=(
  "$SCRIPT_DIR/miner_pid.txt"
  "$SCRIPT_DIR/miner_pid_dev.txt"
  "$SCRIPT_DIR/node_pid_dev.txt"
  "$SCRIPT_DIR/node_pid.txt"
  "$SCRIPT_DIR/node_pid_full_fast.txt"
  "$SCRIPT_DIR/node_pid_archive.txt"
)

# Seconds to wait after SIGTERM before SIGKILL. The node flushes and closes
# its databases on SIGTERM and may need minutes on a slow disk; killing it
# after 5 s, as this script used to, turned nearly every stop into a crash.
NODE_STOP_TIMEOUT="${NODE_STOP_TIMEOUT:-300}"
MINER_STOP_TIMEOUT="${MINER_STOP_TIMEOUT:-10}"

kill_pid() {
  local pid="$1"
  local name="$2"
  local timeout="${3:-$NODE_STOP_TIMEOUT}"
  if ! kill -0 "$pid" >/dev/null 2>&1; then
    echo "No process found with PID: $pid ($name)"
    return 1
  fi
  kill "$pid" >/dev/null 2>&1 || true
  local start=$SECONDS
  local end=$((SECONDS + timeout))
  local next_report=$((SECONDS + 10))
  while kill -0 "$pid" >/dev/null 2>&1; do
    if (( SECONDS >= end )); then
      kill -9 "$pid" >/dev/null 2>&1 || true
      echo "Killed process with PID: $pid ($name) with SIGKILL after ${timeout}s"
      return 0
    fi
    if (( SECONDS >= next_report )); then
      echo "Waiting for PID $pid ($name) to shut down cleanly... $((SECONDS - start))s"
      next_report=$((SECONDS + 10))
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
      if [[ "$PID_FILE" == *miner* ]]; then
        kill_pid "$PID" "$PID_FILE" "$MINER_STOP_TIMEOUT" || true
      else
        kill_pid "$PID" "$PID_FILE" || true
      fi
    else
      echo "Empty PID file: $PID_FILE"
    fi
    rm -f "$PID_FILE"
    echo "Removed $PID_FILE"
  fi
done

# ALWAYS sweep for any remaining node/miner binaries, even if some PID files
# were found. The per-type start scripts use different PID files
# (node_pid_full_fast.txt / node_pid_archive.txt), and a leftover miner_pid.txt
# must not cause us to skip killing a still-running node. Match the binary by
# name so we never touch this shell.
self=$$
for pid in $(pgrep -x mazze-miner 2>/dev/null); do
  [[ "$pid" == "$self" ]] && continue
  kill_pid "$pid" "sweep" "$MINER_STOP_TIMEOUT" || true
done
for pid in $(pgrep -x mazze 2>/dev/null); do
  [[ "$pid" == "$self" ]] && continue
  kill_pid "$pid" "sweep" || true
done

echo "Stop process completed."
