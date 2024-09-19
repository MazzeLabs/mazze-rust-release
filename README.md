# Mazze-Rust

Mazze-rust is a Rust-based implementation of the Mazze protocol. It is fast and reliable.

## Setup Instructions

### 1. Install Prerequisites
```bash
sudo apt update
sudo apt install build-essential pkg-config libssl-dev cmake
# Install Rust: https://www.rust-lang.org/tools/install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```


### 2. Modify Configurations

- Edit `hydra.toml`:
  - Set `mining_author`: hex address of the mining author
  - Set `public_address` public IP address of the node
- Edit `start-miner.sh`:
  - Set `--worker-id`: numeric value used for identifying the worker instance
  - Set `--num-threads`: number of threads to use for mining

### 3. Open Firewall Ports
```bash
sudo ufw allow 32525 # stratum
sudo ufw allow 55555 # p2p
sudo ufw allow 52535 # jsonrpc_ws_port
sudo ufw allow 52536 # jsonrpc_tcp_port
sudo ufw allow 52537 # jsonrpc_http_port
sudo ufw allow 58545 # jsonrpc_http_eth_port
sudo ufw allow 58546 # jsonrpc_ws_eth_port
```


### 4. Build and run
```bash
cargo build --release
cd run && ./start-node.sh && ./start-miner.sh 
```

### 5. Inspect logs

The node and worker will output log files in `run/logs/`, one file for each process. The name of each log includes the timestamp when the process has been launched.
To inspect the logs, we suggest using `tail` on the node/miner logs as follows:
```bash
# For the node
# Replace <timestamp> with the actual file name timestamp (make sure to select the latest log file)

tail -f run/logs/mazze-node-<timestamp>.txt

# For the mining worker
# Replace <timestamp> with the actual file name timestamp (make sure to select the latest log file)

tail -f run/logs/mazze-miner-<timestamp>.txt
```




## License

[GNU General Public License v3.0](https://github.com/s94130586/mazze-rust/blob/master/LICENSE)
