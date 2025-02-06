#!/bin/bash

# Configuration
DOCKER_USERNAME="mazzelabs"
REPO_NAME="mazze-chain"
DOCKER_REGISTRY="${DOCKER_USERNAME}/${REPO_NAME}"
BASE_DIR=$(pwd)

# CPU targets for optimization
TARGETS=(
    "x86-64"  # Most compatible
#    "x86-64-v2"  # Mid-tier
 #   "x86-64-v3"  # Modern CPUs
)

# Initial setup and cleanup
cleanup() {
    echo "Cleaning up temporary directories..."
    rm -rf config/
    rm -rf context_*/
    rm -rf builds/
}

# Setup initial directories and files
setup() {
    echo "Setting up build environment..."
    
    # Clean up any existing temporary files
    cleanup
    
    # Create necessary directories
    mkdir -p logs
    mkdir -p builds
    mkdir -p config
    
    # Copy config files from ../run
    if [ ! -f "../run/hydra.toml" ]; then
        echo "Error: hydra.toml not found in ../run directory"
        exit 1
    fi

    if [ ! -f "./scripts/start-node.sh" ]; then
        echo "Error: start-node.sh not found in ../run directory"
        exit 1
    fi

    if [ ! -f "./scripts/start-miner.sh" ]; then
        echo "Error: start-miner.sh not found in ../run directory"
        exit 1
    fi
    
    cp ../run/hydra.toml config/
    cp ./scripts/start-node.sh config/
    cp ./scripts/start-miner.sh config/
    
    echo "Setup complete. Config files copied."
}

# # Ensure we're logged in to Docker Hub
if ! docker info >/dev/null 2>&1; then
    echo "Error: Docker is not running or not accessible"
    exit 1
fi

if ! docker login >/dev/null 2>&1; then
    echo "Please login to Docker Hub:"
    docker login
fi

# Function to build binaries for a specific target
build_binaries() {
    local target=$1
    echo "Building binaries for CPU target: $target"
    
    # Create target-specific build directory
    local build_dir="builds/$target"
    mkdir -p "$build_dir"
    
    # Set Rust flags for CPU target
    export RUSTFLAGS="-C target-cpu=$target"
    
    # Build in target-specific directory
    echo "Building node and miner for $target..."
    cargo build --release --target-dir "$build_dir"
    
    # Verify binaries exist
    if [ ! -f "$build_dir/release/mazze" ] || [ ! -f "$build_dir/release/mazze-miner" ]; then
        echo "Error: Binary build failed for $target"
        return 1
    fi
}

echo "4. Binaries built"
# Function to build and tag Docker images
build_docker_images() {
    local target=$1
    echo "Building Docker images for CPU target: $target"
    
    # Create temporary build context
    local context_dir="context_$target"
    mkdir -p "$context_dir"
    
    # Copy config and start scripts
    cp -r config "$context_dir/"
    cp ./scripts/start-node.sh "$context_dir/"
    cp ./scripts/start-miner.sh "$context_dir/"
    
    # Copy binaries from correct location
    if [ ! -f "target/$target/mazze" ] || [ ! -f "target/$target/mazze-miner" ]; then
        echo "Error: Binaries not found in target/$target/"
        return 1
    fi
    
    cp "target/$target/mazze" "$context_dir/"
    cp "target/$target/mazze-miner" "$context_dir/"
    
    # Build node image
    docker build \
        -t "mazze-node:$target" \
        -f Dockerfile.node \
        --build-arg BINARY_PATH="mazze" \
        "$context_dir"
    
    # Build miner image
    docker build \
        -t "mazze-miner:$target" \
        -f Dockerfile.miner \
        --build-arg BINARY_PATH="mazze-miner" \
        "$context_dir"

    # Tag images for Docker Hub
    docker tag "mazze-node:$target" "${DOCKER_REGISTRY}:node-$target"
    docker tag "mazze-miner:$target" "${DOCKER_REGISTRY}:miner-$target"
    
    # Cleanup context
    rm -rf "$context_dir"
}

# Function to push images to Docker Hub
push_images() {
    local target=$1
    echo "Pushing images for CPU target: $target"
    
    if docker images | grep -q "${DOCKER_REGISTRY}:node-$target"; then
        docker push "${DOCKER_REGISTRY}:node-$target"
    else
        echo "Warning: node image for $target not found"
    fi
    
    if docker images | grep -q "${DOCKER_REGISTRY}:miner-$target"; then
        docker push "${DOCKER_REGISTRY}:miner-$target"
    else
        echo "Warning: miner image for $target not found"
    fi
}

# Main build process
echo "Starting build process..."

# Run setup
setup

# Trap cleanup on script exit
trap cleanup EXIT

# Process each target
for target in "${TARGETS[@]}"; do
    echo "Processing target: $target"
#    if build_binaries "$target"; then
        build_docker_images "$target"
        push_images "$target"
#    else
#        echo "Skipping Docker build for $target due to binary build failure"
#    fi
done

echo "Build process complete!"echo "Available images:"
docker images | grep "${DOCKER_REGISTRY}"

# Create or update version file
echo "$(date '+%Y-%m-%d %H:%M:%S')" > version.txt
echo "Latest build completed successfully" >> version.txt
echo "CPU targets: ${TARGETS[*]}" >> version.txt

echo "
Next steps:
1. Test the images:
   docker run -d --name test-node ${DOCKER_REGISTRY}:node-x86-64
   docker run -d --name test-miner ${DOCKER_REGISTRY}:miner-x86-64

2. Update your deployment scripts to use:
   ${DOCKER_REGISTRY}:node-x86-64
   ${DOCKER_REGISTRY}:miner-x86-64
"
