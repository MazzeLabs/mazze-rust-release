#!/bin/bash

NODE_PID_FILE="node_pid.txt"
MINER_PID_FILE="miner_pid.txt"

for PID_FILE in "$NODE_PID_FILE" "$MINER_PID_FILE"; do
  if [ -f "$PID_FILE" ]; then
    PID=$(cat "$PID_FILE")
    if kill -0 "$PID" > /dev/null 2>&1; then
      kill "$PID"
      echo "Killed process with PID: $PID from $PID_FILE"
    else
      echo "No process found with PID: $PID from $PID_FILE"
    fi
    rm -f "$PID_FILE"
    echo "Removed $PID_FILE"
  else
    echo "$PID_FILE not found!"
  fi
done

echo "Stop process completed."