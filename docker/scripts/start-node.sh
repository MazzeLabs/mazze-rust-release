#!/bin/bash

ulimit -n 10000

# Create logs directory if it doesn't exist
mkdir -p /app/logs

# Update config file with environment variables if provided
if [ ! -z "$PUBLIC_ADDRESS" ]; then
    echo "Setting public_address to: $PUBLIC_ADDRESS"
    sed -i "s|^public_address=.*|public_address=\"$PUBLIC_ADDRESS\"|" /app/config/hydra.toml
fi

if [ ! -z "$MINING_AUTHOR" ]; then
    echo "Setting mining_author to: $MINING_AUTHOR"
    sed -i "s|^mining_author=.*|mining_author=\"$MINING_AUTHOR\"|" /app/config/hydra.toml
fi

# Debug: Show the current config
echo "Current config file contents:"
cat /app/config/hydra.toml

# Add a timestamp line to the log file
echo "-------$(date '+%Y-%m-%d %H:%M:%S')-------" >> "/app/logs/mazze-node.log"

# Start the node
exec /app/mazze --config /app/config/hydra.toml 2>&1 | tee /app/logs/mazze-node.log