# Mazze Node

Mazze Node is a high-performance implementation of the Mazze protocol in Rust, designed for speed, reliability, and security in blockchain operations.

## Features

- **High Performance**: Built with Rust for maximum efficiency and safety
- **Modular Architecture**: Easy to extend and maintain
- **Smart Contract Support**: Full EVM compatibility
- **Mining Support**: Built-in Stratum protocol implementation
- **Secure**: Advanced cryptography and secure key management
- **Scalable**: Designed for high transaction throughput

## Quick Start

For detailed setup instructions, see our [Node & Miner Setup Guide](docs/setup-guide.md).

### Prerequisites

- Rust (latest stable version)
- Cargo (Rust's package manager)
- RocksDB development libraries
- Clang
- Make
- Git

### Building from Source

```bash
# Clone the repository
git clone https://github.com/MazzeLabs/mazze-rust-release
cd mazze-rust-release

# Build the project
cargo build --release
```

## Project Structure

```
mazze-rust-release/
├── bins/                  # Binary targets
│   ├── mazze/            # Main node implementation
│   ├── mazze-miner/      # Mining client
│   ├── mazze-key/        # Key management utility
│   └── mazze-store/      # Storage management
├── crates/               # Core libraries
│   ├── accounts/         # Account management
│   ├── blockgen/         # Block generation
│   ├── mazzecore/        # Core blockchain logic
│   ├── network/          # P2P networking
│   └── ...               # Other utility crates
```

## Running the Node

To start a Mazze node:

```bash
cargo run --release --bin mazze -- [OPTIONS]
```

## Mining

To start mining with the built-in miner:

```bash
cargo run --release --bin mazze-miner -- [OPTIONS]
```

## Key Management

Use the key management utility to create and manage keys:

```bash
cargo run --release --bin mazze-key -- [COMMAND]
```

## RPC API

Mazze Node provides a comprehensive JSON-RPC API for interacting with the blockchain. The API is available over HTTP, WebSocket, and IPC.

### Available Namespaces

#### 1. Mazze Namespace
- `mazze_getBalance` - Get balance of an account
- `mazze_getTransactionCount` - Get transaction count for an account
- `mazze_blockNumber` - Get current block number
- `mazze_getBlockByHash` - Get block by hash
- `mazze_getBlockByNumber` - Get block by number
- `mazze_getTransactionByHash` - Get transaction by hash
- `mazze_getTransactionReceipt` - Get transaction receipt
- `mazze_sendRawTransaction` - Send a signed transaction
- `mazze_call` - Execute a message call
- `mazze_estimateGas` - Estimate gas for a transaction
- `mazze_getLogs` - Get logs matching a filter

#### 2. Debug Namespace
- `debug_traceTransaction` - Trace a transaction's execution
- `debug_traceBlockByHash` - Trace all transactions in a block
- `debug_traceBlockByNumber` - Trace all transactions in a block
- `debug_traceCall` - Trace a call

#### 3. Trace Namespace
- `trace_block` - Get all traces produced at a given block
- `trace_transaction` - Get all traces produced at a given transaction
- `trace_filter` - Get all traces matching a filter

#### 4. TxPool Namespace
- `txpool_status` - Get transaction pool status
- `txpool_content` - Get transaction pool content
- `txpool_nextNonce` - Get next nonce for an account
- `txpool_accountPendingInfo` - Get pending transactions for an account

### Example Usage

#### Using cURL

```bash
# Get current block number
curl -X POST --data '{"jsonrpc":"2.0","method":"mazze_blockNumber","params":[],"id":1}' http://localhost:8545

# Get balance of an account
curl -X POST --data '{"jsonrpc":"2.0","method":"mazze_getBalance","params":["0x...", "latest"],"id":1}' http://localhost:8545
```

#### Using WebSocket

```javascript
const WebSocket = require('ws');
const ws = new WebSocket('ws://localhost:8546');

ws.on('open', function open() {
  ws.send(JSON.stringify({
    jsonrpc: "2.0",
    id: 1,
    method: "mazze_subscribe",
    params: ["newHeads"]
  }));
});

ws.on('message', function incoming(data) {
  console.log('New block:', JSON.parse(data));
});
```

### Authentication

For production deployments, it's recommended to enable authentication. The RPC server supports HTTP basic authentication and JWT tokens.

## License

This project is licensed under the terms specified in the [LICENSE](LICENSE) file.

## Contributing

Contributions are welcome! Please read our contributing guidelines before submitting pull requests.

## Security

For security-related issues, please contact the development team directly.