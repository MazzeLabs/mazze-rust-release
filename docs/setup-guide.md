# Mazze Node & Miner Setup Guide

This guide describes two methods for setting up a Mazze node and miner: using Docker (recommended) or building from source. For the Zurich development phase, we recommend using Docker.

For reading logs, see our [Viewing Mazze Logs](viewing-logs.md) guide.

## 1. Docker Setup

### 1.1 Compose-based Setup (Recommended)

The repository includes a ready-to-use `docker-compose.yml`. To run Mazze:

1. Edit `run/hydra.toml`:
   - Set `public_address = "<your-public-ip>"` (or leave empty to auto-detect).
   - Set `mining_author = "<your-base32-mazze-address>"` (or leave empty to disable mining on the node).
   - Ensure `log_conf = "/app/config/log.yaml"` is present (already set in this repo).

2. Optional: edit `run/log.yaml` for logging format/level. It is mounted into the container at `/app/config/log.yaml` and has `refresh_rate: 30 seconds`.

3. Start the services:
```bash
sudo docker compose up -d
```

4. View logs:
```bash
sudo docker compose logs node | tail -n 200
sudo docker compose logs miner | tail -n 200
```

5. Retrieve your node ID (after node starts):
```bash
sudo docker compose logs node | grep "Self node id:" | tail -n 1
```

6. Apply config changes:
```bash
sudo docker compose up -d --force-recreate
```

7. Stop services:
```bash
sudo docker compose down
```

Notes:
- Logs are written under `./logs/node` and `./logs/miner` on the host.
- For persistent chain data on the host, you can add a volume to `docker-compose.yml`, for example:
  - `- ./blockchain_data:/app/blockchain_data`


### 1.2 Manual Docker Run (Optional)

If you prefer `docker run`, ensure ports are open and set the same mounts as in the compose file. Compose is recommended for simplicity.



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

## Important Notes
Changing log level / config:

- Edit `run/hydra.toml` (`log_level = "info" | "debug" | "warn" | "error"`) or adjust `run/log.yaml`.
- Then recreate containers:
```bash
sudo docker compose up -d --force-recreate
```

Container logging options are handled by Docker; compose already mounts `run/log.yaml` as `/app/config/log.yaml` with a refresh rate, so updates apply without rebuilding images.

## Additional Notes

- Ensure Docker is installed and running on your system
- The automated setup creates a `node_id.txt` file containing your node's identifier
- Monitor the logs directory for debugging information
- For security reasons, consider configuring additional firewall rules
- Backup your mining author address securely