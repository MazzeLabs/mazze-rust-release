#!/usr/bin/env bash
# Start a Mazze node tuned as a "full-fast" miner / fast-bootstrap node.
#
# Profile rationale:
# - node_type = "full-fast" triggers the in-code aggressive sync profile
#   (raises max_outgoing_peers >=32, max_inflight_request_count >=2000,
#   storage_max_open_snapshots >=16, paritydb_max_open_files >=1024, etc.).
# - persist_*_index = false: miners don't need RPC lookups by block-number /
#   tx-hash; saves disk and write I/O on every block.
# - Smaller ledger_cache_size: no heavy log scans, recover that RAM.
# - snapshot_epoch_count + provide_more_snapshot_for_sync aligned so a fresh
#   miner can bootstrap from the nearest checkpoint instead of replaying.
# - sync_state_epoch_gap >= snapshot_epoch_count starts state sync as soon
#   as the node is one snapshot behind, minimising replay tail.
# - Mining keys (mining_author / stratum_*) stay in hydra.toml so each host
#   keeps its own identity.
set -euo pipefail

ulimit -n 200000 || true

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
EXECUTABLE="$REPO_ROOT/target/release/mazze"
CONFIG_FILE="$SCRIPT_DIR/hydra.toml"
LOG_DIR="$REPO_ROOT/run/logs"
PID_FILE="$SCRIPT_DIR/node_pid_full_fast.txt"
LOG_FILE="$LOG_DIR/mazze-node-full-fast.log"
TEMP_CONF="$SCRIPT_DIR/hydra.full-fast.runtime.toml"
ABS_LOG_CONF="$SCRIPT_DIR/log.yaml"

mkdir -p "$LOG_DIR" "$LOG_DIR/archive"

if [[ ! -x "$EXECUTABLE" ]]; then
  echo "Error: binary not found at $EXECUTABLE. Did you run: cargo build --release?" >&2
  exit 1
fi

if [[ ! -f "$CONFIG_FILE" ]]; then
  echo "Error: config not found at $CONFIG_FILE" >&2
  exit 1
fi

export MAZZE_SHIELDED_VK_HEX="${MAZZE_SHIELDED_VK_HEX:-$SCRIPT_DIR/shielded_vk.hex}"

cp "$CONFIG_FILE" "$TEMP_CONF"

set_kv() {
  local key="$1"
  local value="$2"
  if grep -q "^[[:space:]]*${key}[[:space:]]*=" "$TEMP_CONF"; then
    sed -i "s#^[[:space:]]*${key}[[:space:]]*=.*#${key} = ${value}#" "$TEMP_CONF"
  else
    printf '\n%s = %s\n' "$key" "$value" >> "$TEMP_CONF"
  fi
}

# --- Full-fast profile -----------------------------------------------------
set_kv "node_type"                            "\"full-fast\""
set_kv "log_conf"                             "\"$ABS_LOG_CONF\""

# Disable RPC indices — miners don't serve historical RPC.
set_kv "persist_block_number_index"           "false"
set_kv "persist_tx_index"                     "false"

# Smaller cache; the RAM is better spent on snapshot/MPT pages.
set_kv "ledger_cache_size"                    "2048"

# RPC threads can stay low (monitoring / health only). The full-fast profile
# itself doesn't touch this — set it explicitly so we don't waste CPU.
set_kv "jsonrpc_http_threads"                 "2"

# Snapshot alignment: lets a freshly-started miner bootstrap from the latest
# checkpoint rather than replaying from genesis.
set_kv "snapshot_epoch_count"                 "2048"
set_kv "provide_more_snapshot_for_sync"       "\"checkpoint,multiple_of_2048\""
set_kv "sync_state_epoch_gap"                 "2048"

# Don't request blocks-with-public during catchup; full-fast profile flips
# this to true automatically, but on CPU-bound miners the smaller payload is
# usually a win — comment this out if your bottleneck is CPU not bandwidth.
set_kv "request_block_with_public"            "true"

# Storage
set_kv "state_db_type"                        "\"mdbx\""
set_kv "mdbx_map_size_mb"                     "65536"
set_kv "mdbx_max_readers"                     "1024"
set_kv "mdbx_sync_mode"                       "\"relaxed\""

# Sanity-check: mining_author should be set on a miner node.
if ! grep -qE '^[[:space:]]*mining_author[[:space:]]*=[[:space:]]*"[^"]+"' "$TEMP_CONF"; then
  echo "Warning: mining_author is not set in hydra.toml — this node won't mine." >&2
  echo "         Set 'mining_author = \"mazze:...\"' in $CONFIG_FILE." >&2
fi

has_chain_data() {
  local base="$1"
  compgen -G "$base/blockchain_db/*" > /dev/null && return 0
  compgen -G "$base/storage_db/*"    > /dev/null && return 0
  compgen -G "$base/net_config/*"    > /dev/null && return 0
  return 1
}
if has_chain_data "$REPO_ROOT/blockchain_data"; then
  set_kv "execute_genesis" "false"
fi

# Storage Phase 3 — enable all 7 MDBX shadow mirrors so every write is
# dual-written to MDBX alongside ParityDB from genesis. Required before reads
# can be cut over to MDBX via debug_mdbxSetReadSource(table, "shadow").
for _f in enable_mdbx_shadow_hash_by_number enable_mdbx_shadow_tx_index \
          enable_mdbx_shadow_blamed_header_verified_roots \
          enable_mdbx_shadow_block_traces enable_mdbx_shadow_blocks \
          enable_mdbx_shadow_epoch_numbers enable_mdbx_shadow_misc; do
  if grep -q "^[[:space:]]*${_f}[[:space:]]*=" "$TEMP_CONF"; then
    sed -i "s#^[[:space:]]*${_f}[[:space:]]*=.*#${_f} = true#" "$TEMP_CONF"
  else
    printf '\n%s = true\n' "$_f" >> "$TEMP_CONF"
  fi
done

echo "-------$(date '+%Y-%m-%d %H:%M:%S') [full-fast]-------" >> "$LOG_FILE"

pushd "$REPO_ROOT" >/dev/null
"$EXECUTABLE" --config "$TEMP_CONF" >> "$LOG_FILE" 2>&1 &
PID=$!
echo "$PID" > "$PID_FILE"
echo "Mazze full-fast node started (pid=$PID). Config: $TEMP_CONF  Logs: $LOG_FILE"
popd >/dev/null
