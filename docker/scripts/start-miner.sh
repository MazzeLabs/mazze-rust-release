#!/bin/bash

ulimit -n 10000

# Create logs directory if it doesn't exist
mkdir -p /app/logs

# Update config file with environment variables if provided
if [ ! -z "$MINING_AUTHOR" ]; then
    echo "Setting mining_author to: $MINING_AUTHOR"
    sed -i "s|^mining_author=.*|mining_author=\"$MINING_AUTHOR\"|" /app/config/hydra.toml
fi

# Debug: Show the current config
echo "Current config file contents:"
cat /app/config/hydra.toml

# Set default values for stratum connection if not provided
STRATUM_HOST=${STRATUM_HOST:-"mazze-node"}
STRATUM_PORT=${STRATUM_PORT:-32525}

# Add a timestamp line to the log file
echo "-------$(date '+%Y-%m-%d %H:%M:%S')-------" >> "/app/logs/mazze-miner.log"

# Run the miner with command-line parameters
RUST_LOG=info exec /app/mazze-miner \
    --config /app/config/hydra.toml \
    --worker-id "${WORKER_ID:-1}" \
    --num-threads "${NUM_THREADS:-4}" \
    --stratum-address "${STRATUM_HOST}:${STRATUM_PORT}" \
    2>&1 | tee /app/logs/mazze-miner.log