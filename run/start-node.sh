#!/bin/bash

ulimit -n 10000

EXECUTABLE="./../target/release/mazze"
PID_FILE="node_pid.txt"

# Create logs directory if it doesn't exist
mkdir -p logs

MAZZE_NODE_LOG_FILE="./logs/mazze-node-$(date +%Y-%m-%d_%H:%M:%S).txt"
$EXECUTABLE --config hydra.toml > "$MAZZE_NODE_LOG_FILE" 2>&1 &

PID=$!

echo "Mazze is running with PID: $PID"

echo $PID > $PID_FILE

echo "Mazze node has been started in the background."