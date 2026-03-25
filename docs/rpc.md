# Mazze JSON-RPC Guide

Mazze exposes JSON-RPC over HTTP, WebSocket, and TCP. There are two RPC
surfaces:
- Mazze core (native) APIs.
- EVM (Ethereum-compatible) APIs.

The exact set of enabled methods depends on your config.

## Endpoints and configuration
All RPC ports and API allowlists live in `run/hydra.toml`.

Common fields:
- `jsonrpc_http_port`, `jsonrpc_ws_port`, `jsonrpc_tcp_port`: Mazze core APIs.
- `jsonrpc_local_http_port`, `jsonrpc_local_ws_port`, `jsonrpc_local_tcp_port`:
  local-only Mazze endpoints (recommended for debug/admin calls).
- `jsonrpc_http_eth_port`, `jsonrpc_ws_eth_port`: EVM APIs.
- `public_rpc_apis`: core API allowlist (`all`, `safe`, `mazze`, `debug`,
  `trace`, `txpool`, `test`, `pubsub`, ...).
- `public_evm_rpc_apis`: EVM API allowlist (`evm`, `eth`, `ethpubsub`,
  `ethdebug`).

## HTTP call examples
Mazze core status:
```bash
curl -s http://127.0.0.1:12539 \
  -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"mazze_getStatus","params":[]}'
```

The response keeps the legacy top-level fields for compatibility and now also
includes grouped `progress`, `randomx`, `snapshots`, and `era` objects. For operator
dashboards, prefer:
- `progress.bestEpochNumber`
- `progress.bestBlockNumber`
- `progress.latestStateEpochNumber`
- `randomx.epochNumber`
- `snapshots.latestSnapshotEpochNumber`
- `snapshots.availableSnapshotCount`
- `era.number`

`mazze_getRandomXEpochInfo` now uses:
- `epochNumber`: RandomX epoch index
- `selectedEpochNumber`: Mazze epoch used to resolve the query
- `startEpochNumber` / `endEpochNumber` / `nextTransitionEpochNumber`

The older `startBlockHeight` / `endBlockHeight` / `nextTransitionBlockHeight`
fields are kept as legacy aliases.

Mazze call (latest_state):
```bash
curl -s http://127.0.0.1:12539 \
  -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"mazze_call","params":[{"to":"MAZZE:TYPE.BUILTIN:AAEJUAAAAAAAAAAAAAAAAAAAAAAAAAAABAJ1SJV3W2","data":"0xebf0c717"},"latest_state"]}'
```

EVM (eSpace) block number:
```bash
curl -s http://127.0.0.1:58545 \
  -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}'
```

## WebSocket pubsub example
```bash
# Requires a WS client like wscat
wscat -c ws://127.0.0.1:52535
> {"jsonrpc":"2.0","id":1,"method":"mazze_subscribe","params":["newHeads"]}
```

## Supply and burn calls
- `mazze_getSupplyInfo`: returns chain supply aggregates from node state.
- `mazze_getFeeBurnt`: returns cumulative MIP-1559 burned amount.

Interpretation note:
- Theoretical tokenomics maximum and live bridge liquidity are separate from
  direct RPC state values.
- For current tokenomics model and bridge constraints, see:
  `docs/tokenomics/README.md` and `docs/tokenomics/bridge-liquidity.md`.

## Full RPC method list

### Mazze core (native) APIs
```text
mazze_gasPrice
mazze_maxPriorityFeePerGas
mazze_epochNumber
mazze_getBalance
mazze_getAdmin
mazze_getSponsorInfo
mazze_getCollateralForStorage
mazze_getCode
mazze_getStorageAt
mazze_getStorageRoot
mazze_getBlockByHash
mazze_getBlockByHashWithMainAssumption
mazze_getBlockByEpochNumber
mazze_getBlockByBlockNumber
mazze_getBestBlockHash
mazze_getNextNonce
mazze_sendRawTransaction
mazze_call
mazze_getLogs
mazze_getTransactionByHash
mazze_getAccountPendingInfo
mazze_getAccountPendingTransactions
mazze_estimateGasAndCollateral
mazze_feeHistory
mazze_checkBalanceAgainstTransaction
mazze_getBlocksByEpoch
mazze_getSkippedBlocksByEpoch
mazze_getTransactionReceipt
mazze_getAccount
mazze_getConfirmationRiskByHash
mazze_getStatus
mazze_getBlockRewardInfo
mazze_clientVersion
mazze_getSupplyInfo
mazze_getCollateralInfo
mazze_getFeeBurnt
mazze_isTimerBlock
mazze_getTimerChain
mazze_getTimerChainDifficulty
mazze_getBlockBlameInfo
mazze_getDagTips
mazze_getSkippedBlockHashesByEpoch
mazze_getRandomXEpochInfo
mazze_getEraDetails
mazze_getBlockWeight
mazze_isAdaptiveBlock
mazze_isPartialInvalid
```

### Mazze filters
```text
mazze_newFilter
mazze_newBlockFilter
mazze_newPendingTransactionFilter
mazze_getFilterChanges
mazze_getFilterLogs
mazze_uninstallFilter
```

### Mazze txpool
```text
txpool_status
txpool_nextNonce
txpool_transactionByAddressAndNonce
txpool_pendingNonceRange
txpool_txWithPoolInfo
txpool_accountPendingInfo
txpool_accountPendingTransactions
```

### Mazze debug and admin
```text
txpool_inspect
txpool_content
txpool_accountTransactions
txpool_clear
net_throttling
net_node
net_disconnect_node
net_sessions
current_sync_phase
consensus_graph_state
sync_graph_state
mazze_sendTransaction
accounts
new_account
unlock_account
lock_account
sign
mazze_signTransaction
mazze_getEpochReceipts
debug_statOnGasLoad
debug_getEpochReceiptProofByTransaction
debug_getTransactionsByEpoch
debug_getTransactionsByBlock
```

### Mazze trace
```text
trace_block
trace_filter
trace_transaction
trace_epoch
```

### Mazze test/dev
```text
sayhello
getblockcount
getgoodput
generate_empty_blocks
generatefixedblock
addnode
removenode
getpeerinfo
mazze_getChain
stop
getnodeid
addlatency
generateoneblock
generate_one_block_with_direct_txgen
test_generatecustomblock
test_generateblockwithfaketxs
test_generateblockwithblameinfo
test_generate_block_with_nonce_and_timestamp
get_block_status
expireblockgc
getMainChainAndWeight
getExecutedInfo
test_sendUsableGenesisAccounts
set_db_crash
save_node_db
```

### Mazze pubsub (WebSocket)
Methods:
```text
mazze_subscribe
mazze_unsubscribe
```
Subscription kinds: `newHeads`, `logs`, `newPendingTransactions`, `syncing`,
`epochs`.

### EVM (Ethereum-compatible) APIs
```text
web3_clientVersion
net_version
eth_protocolVersion
eth_syncing
eth_hashrate
eth_coinbase
eth_mining
eth_chainId
eth_gasPrice
eth_maxPriorityFeePerGas
eth_feeHistory
eth_accounts
eth_blockNumber
eth_getBalance
eth_getStorageAt
eth_getBlockByHash
eth_getBlockByNumber
eth_getTransactionCount
eth_getBlockTransactionCountByHash
eth_getBlockTransactionCountByNumber
```
