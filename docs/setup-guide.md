# Mazze Node & Miner Setup Guide

This guide describes two methods for setting up a Mazze node and miner: using Docker (recommended) or building from source. For the Zurich development phase, we recommend using Docker.

For reading logs, see our [Viewing Mazze Logs](viewing-logs.md) guide.

## 1. Docker Setup

### 1.1 Automated Setup (Recommended)

Create the following three script files in your working directory:

#### cleanup.sh
Used for chain restarts during testnet (not applicable after mainnet launch):
```bash
rm -rf /opt/mazze/blockchain_data/blockchain_db /opt/mazze/blockchain_data/storage_db
```

#### constants.sh
Configuration file for initial node and miner settings:
```bash
#!/bin/bash
PUBLIC_IP="" # Your VM's external IP address
MINING_AUTHOR="" # Your base32 MAZZE address
WORKER_ID="1" # Unique identifier for multiple mining rigs
NUM_THREADS="4" # Recommended: (CPU_CORES / 2) - 2
```

#### setup_mazze.sh
Main setup script that configures firewall rules and launches containers:

```bash
#!/bin/bash
source constants.sh

# Configure firewall
setup_firewall() {
    echo "Opening required ports..."
    sudo ufw allow 55555/tcp
    sudo ufw allow 32525/tcp
    sudo ufw allow 52535/tcp
    sudo ufw allow 52536/tcp
    sudo ufw allow 52537/tcp
    sudo ufw allow 58545/tcp
    sudo ufw allow 58546/tcp
    sudo ufw reload
}

# Main setup function
setup_node() {
    local public_ip=$1
    
    # Create required directories
    mkdir -p ./logs
    
    # Start node container
    docker run -d \
        --name mazze-node \
        --network host \
        -v "$(pwd)/logs:/app/logs" \
        -v /opt/mazze/blockchain_data:/app/blockchain_data \
        -e PUBLIC_ADDRESS="$public_ip" \
        -e MINING_AUTHOR="$MINING_AUTHOR" \
        mazzelabs/mazze-chain:node-x86-64

    # Start miner container
    docker run -d \
        --name mazze-miner \
        -v "$(pwd)/logs:/app/logs" \
        -e MINING_AUTHOR="$MINING_AUTHOR" \
        -e WORKER_ID="$WORKER_ID" \
        -e NUM_THREADS="$NUM_THREADS" \
        -e STRATUM_HOST="$public_ip" \
        mazzelabs/mazze-chain:miner-x86-64
        
    # Wait for node to start and extract node ID
    echo "Waiting for node to initialize..."
    local max_attempts=30
    local attempt=1
    local node_id=""
    
    while [ $attempt -le $max_attempts ]; do
        echo "Attempting to fetch node ID (attempt $attempt/$max_attempts)..."
        node_id=$(docker logs mazze-node 2>&1 | grep "Self node id:" | sed -E '"'"'s/.*Self node id: (0x[a-f0-9]+).*/\1/'"'"')
        
        if [ ! -z "$node_id" ]; then
            echo "Successfully retrieved node ID:"
            echo "$node_id"
            # Save node ID to a file
            echo "$node_id" > node_id.txt
            break
        fi
        
        sleep 10
        ((attempt++))
    done
    
    if [ -z "$node_id" ]; then
        echo "Failed to retrieve node ID after $max_attempts attempts"
        exit 1
    fi
}

# Main execution
setup_firewall

# Execute main setup with parameters
setup_node "$PUBLIC_IP"
```


### 1.2 Manual Setup

For advanced users who need more control over the configuration:

1. Configure firewall rules:
```bash
    sudo ufw allow 55555/tcp
    sudo ufw allow 32525/tcp
    sudo ufw allow 52535/tcp
    sudo ufw allow 52536/tcp
    sudo ufw allow 52537/tcp
    sudo ufw allow 58545/tcp
    sudo ufw allow 58546/tcp
    sudo ufw reload
```

2. Launch the node container:
```bash
    docker run -d \
        --name mazze-node \
        --network host \
        -v "$(pwd)/logs:/app/logs" \
        -v /opt/mazze/blockchain_data:/app/blockchain_data \
        -e PUBLIC_ADDRESS="$public_ip" \
        -e MINING_AUTHOR="$MINING_AUTHOR" \
        mazzelabs/mazze-chain:node-x86-64
```

3. Launch the miner container:
```
    docker run -d \
        --name mazze-miner \
        -v "$(pwd)/logs:/app/logs" \
        -e MINING_AUTHOR="$MINING_AUTHOR" \
        -e WORKER_ID="$WORKER_ID" \
        -e NUM_THREADS="$NUM_THREADS" \
        -e STRATUM_HOST="$public_ip" \
        mazzelabs/mazze-chain:miner-x86-64
```



## 2. Building from Source

For developers who want to build from source:

1. Clone the repository:
```bash
git clone https://github.com/MazzeLabs/mazze-rust-release.git
cd mazze-rust-release
```

2. Install dependencies:
```bash
sudo apt-get update
sudo apt-get install build-essential pkg-config libssl-dev cmake hwloc libhwloc-dev libudev-dev
# Install Rust: https://www.rust-lang.org/tools/install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```


3. Build the project:
```bash
cargo build --release
```


4. Configure the node:
   - Edit `run/hydra.toml`
   - Set your VM's IP address
   - Configure your mining author address

5. Start the node and miner:
```bash
./start-node.sh
./start-miner.sh
```

## Additional Notes

- Ensure Docker is installed and running on your system
- The automated setup creates a `node_id.txt` file containing your node's identifier
- Monitor the logs directory for debugging information
- For security reasons, consider configuring additional firewall rules
- Backup your mining author address securely