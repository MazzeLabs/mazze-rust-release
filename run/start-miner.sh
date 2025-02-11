#!/bin/bash

ulimit -n 10000

EXECUTABLE="./../target/release/mazze-miner"
PID_FILE="miner_pid.txt"

# Create logs directory if it doesn't exist
mkdir -p logs

MAZZE_MINER_LOG_FILE="./logs/mazze-miner.log"

# Add a timestamp line to the log file
echo "-------$(date '+%Y-%m-%d %H:%M:%S')-------" >> "$MAZZE_MINER_LOG_FILE"

RUST_LOG=info $EXECUTABLE --config hydra.toml --worker-id 1 --num-threads 16 >> "$MAZZE_MINER_LOG_FILE" 2>&1 &

PID=$!

echo "Miner is running with PID: $PID"

echo $PID > $PID_FILE

echo "Mazze miner has been started in the background."
