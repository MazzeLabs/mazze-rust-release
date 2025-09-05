#!/bin/bash

ulimit -n 10000

# Create logs directory if it doesn't exist
mkdir -p /app/logs

# Resolve config: prefer mounted file, or ENV/URL/B64
mkdir -p /app/config
CONFIG_FILE="${CONFIG_PATH:-/app/config/hydra.toml}"

if [ -n "$CONFIG_URL" ]; then
    echo "Fetching config from URL: $CONFIG_URL"
    curl -fsSL "$CONFIG_URL" -o "$CONFIG_FILE" || {
        echo "Failed to download CONFIG_URL"; exit 1; }
elif [ -n "$CONFIG_BASE64" ]; then
    echo "Decoding config from CONFIG_BASE64"
    echo "$CONFIG_BASE64" | base64 -d > "$CONFIG_FILE" || {
        echo "Failed to decode CONFIG_BASE64"; exit 1; }
elif [ ! -f "$CONFIG_FILE" ]; then
    echo "No config provided. Exiting. Mount a config or set CONFIG_URL/CONFIG_BASE64."
    exit 1
fi

# Work on a writable copy in case CONFIG_FILE is on a read-only mount
EFFECTIVE_CONFIG="/tmp/hydra.toml"
cp "$CONFIG_FILE" "$EFFECTIVE_CONFIG"

# Update config file with environment variables if provided
if [ ! -z "$PUBLIC_ADDRESS" ]; then
    echo "Setting public_address to: $PUBLIC_ADDRESS"
    sed -i "s|^public_address[[:space:]]*=.*|public_address=\"$PUBLIC_ADDRESS\"|" "$EFFECTIVE_CONFIG"
fi

if [ ! -z "$MINING_AUTHOR" ]; then
    echo "Setting mining_author to: $MINING_AUTHOR"
    sed -i "s|^mining_author[[:space:]]*=.*|mining_author=\"$MINING_AUTHOR\"|" "$EFFECTIVE_CONFIG"
fi

# Ensure log config path points to the file inside the image
if [ -f "/app/config/log.yaml" ]; then
    if grep -qE '^log_conf([[:space:]]*)=' "$EFFECTIVE_CONFIG"; then
        sed -i 's|^log_conf[[:space:]]*=.*|log_conf="/app/config/log.yaml"|' "$EFFECTIVE_CONFIG"
    else
        echo 'log_conf="/app/config/log.yaml"' >> "$EFFECTIVE_CONFIG"
    fi
fi

# Debug: Show the current config
echo "Current config file contents ($EFFECTIVE_CONFIG):"
cat "$EFFECTIVE_CONFIG"

# Add a timestamp line to the log file
echo "-------$(date '+%Y-%m-%d %H:%M:%S')-------" >> "/app/logs/mazze-node.log"

# Start the node
exec /app/mazze --config "$EFFECTIVE_CONFIG" 2>&1 | tee /app/logs/mazze-node.log