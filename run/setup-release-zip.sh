#!/bin/bash

# Run cargo build in release mode
cargo build --release

# Create a temporary directory for the release files
temp_dir=$(mktemp -d)

# Copy necessary files to the temporary directory
cp ../target/release/mazze "$temp_dir/"
cp ../target/release/mazze-miner "$temp_dir/"
cp ../run/hydra.toml "$temp_dir/"
cp ../run/start-node.sh "$temp_dir/"
cp ../run/start-miner.sh "$temp_dir/"
cp ../run/stop.sh "$temp_dir/"
# Add more cp commands for any other files you want to include

# Generate the timestamp for the zip file name
timestamp=$(date +"%Y-%m-%d_%H-%M-%S")
zip_file="release-$timestamp.zip"

# Create the zip file
zip -j "$zip_file" "$temp_dir"/*

# Clean up the temporary directory
rm -rf "$temp_dir"

# Print the zip file name
echo "Release zip file created: $zip_file"
