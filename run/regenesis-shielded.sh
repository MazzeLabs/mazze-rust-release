#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

MODE="dev"
KEEP_KEYS="false"

usage() {
  echo "Usage: $0 [--dev|--release] [--keep-keys]" >&2
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dev)
      MODE="dev"
      shift
      ;;
    --release)
      MODE="release"
      shift
      ;;
    --keep-keys)
      KEEP_KEYS="true"
      shift
      ;;
    -h|--help)
      usage
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      ;;
  esac
done

if [[ "$MODE" == "dev" ]]; then
  DATA_DIR="$SCRIPT_DIR/blockchain_data_dev"
  START_SCRIPT="$SCRIPT_DIR/start-node-dev.sh"
else
  DATA_DIR="$SCRIPT_DIR/blockchain_data"
  START_SCRIPT="$SCRIPT_DIR/start-node.sh"
fi

echo "Stopping node..."
"$SCRIPT_DIR/stop.sh" || true

if [[ "$KEEP_KEYS" != "true" ]]; then
  echo "Generating shielded keys..."
  cargo run -p mazze-executor --bin shielded_keygen -- \
    --out "$SCRIPT_DIR/shielded_vk.hex" \
    --out-pk "$SCRIPT_DIR/shielded_pk.hex"
else
  if [[ ! -f "$SCRIPT_DIR/shielded_vk.hex" || ! -f "$SCRIPT_DIR/shielded_pk.hex" ]]; then
    echo "Missing shielded key files; remove --keep-keys to generate them." >&2
    exit 1
  fi
fi

echo "Building node binary..."
if [[ "$MODE" == "dev" ]]; then
  cargo build -p mazze
else
  cargo build -p mazze --release
fi

echo "Removing data dir: $DATA_DIR"
rm -rf "$DATA_DIR"

echo "Starting node ($MODE)..."
"$START_SCRIPT"
