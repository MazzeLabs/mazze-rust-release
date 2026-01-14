#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

RPC_URL="${RPC_URL:-http://127.0.0.1:12539}"
GAS="${GAS:-21000}"
GAS_PRICE="${GAS_PRICE:-1}"
STORAGE_LIMIT="${STORAGE_LIMIT:-0}"
MAZZE_DECIMALS="${MAZZE_DECIMALS:-18}"

MAZZE_HOME="${MAZZE_HOME:-$HOME/.mazze}"
TOOLS_DIR="$MAZZE_HOME/tools"
ADDRCONV_BIN="$TOOLS_DIR/addrconv"

ADDRESS1_EVM_HEX="0xfFD05dc5b53db270b52b4bC2B5068d41CEf1b240"
ADDRESS1_HEX="0x1fd05dc5b53db270b52b4bc2b5068d41cef1b240"
ADDRESS1_BASE32="MAZZE:TYPE.USER:AAT7A1SF0Y85E6FZFRF6FRJGVZA676RWJAFR8MTNAV"
PRIVATEKEY1="a2e4ef9d454646dc1da510de55ce7f3bc42db42feeb0e53243fdc4f09460afbb"
ADDRESS2_EVM_HEX="0xAc5187739fa3d138856be85F416c58Ef98E9B69B"
ADDRESS2_HEX="0x1c5187739fa3d138856be85f416c58ef98e9b69b"
ADDRESS2_BASE32="MAZZE:TYPE.USER:AASFDB5XX8V7CSEFRTYF8UNPND13V4R0XP9TG439H7"
PRIVATEKEY2="d3131c8519fc1745e8a53d00a02680198f0b23bfd16ebd09d1a11f112cb1b2ff"

FROM_SECRET="$PRIVATEKEY1"
TO_INPUT="$ADDRESS2_BASE32"
AMOUNT_MAZZE=""

strip_0x() {
  local value="$1"
  value="${value#0x}"
  value="${value#0X}"
  printf '%s' "$value"
}

is_hex() {
  [[ "$1" =~ ^[0-9a-fA-F]+$ ]]
}

py() {
  if command -v python3 >/dev/null 2>&1; then
    python3 "$@"
  else
    python "$@"
  fi
}

is_amount() {
  [[ "$1" =~ ^0x[0-9a-fA-F]+$ || "$1" =~ ^[0-9]+([.][0-9]+)?$ ]]
}

amount_to_wei() {
  local amount="$1"
  py -c 'import sys,decimal
amount=sys.argv[1].strip()
decimals_raw=sys.argv[2].strip()
try:
    decimals=int(decimals_raw)
    if decimals <= 0:
        decimals=18
except Exception:
    decimals=18
if amount.lower().startswith("0x"):
    try:
        print(int(amount,16))
        sys.exit(0)
    except Exception:
        sys.exit(1)
if "," in amount and "." not in amount:
    amount = amount.replace(",", ".")
try:
    decimal.getcontext().prec = max(50, len(amount) + decimals + 5)
    dec = decimal.Decimal(amount)
except Exception:
    sys.exit(1)
if dec.is_signed():
    sys.exit(1)
scale = decimal.Decimal(10) ** decimals
wei = dec * scale
if wei != wei.to_integral_value():
    sys.exit(1)
print(int(wei))' "$amount" "$MAZZE_DECIMALS"
}

is_privkey() {
  local value
  value="$(strip_0x "$1")"
  [[ ${#value} -eq 64 ]] && is_hex "$value"
}

is_address() {
  local value="$1"
  if [[ "$value" == 0x* || "$value" == 0X* ]]; then
    value="$(strip_0x "$value")"
    [[ ${#value} -eq 40 ]] && is_hex "$value"
    return $?
  fi
  if [[ "$value" == *:* || "$value" == mazze:* || "$value" == MAZZE:* ]]; then
    return 0
  fi
  return 1
}

usage() {
  echo "Usage: $0 [from-secret-hex] [to-base32] <amount-mazze>" >&2
  echo "Short forms:" >&2
  echo "  $0 <amount-mazze>" >&2
  echo "  $0 <from-secret-hex> <amount-mazze>" >&2
  echo "  $0 <to-base32> <amount-mazze>" >&2
  echo "Defaults (native): from=$ADDRESS1_BASE32 to=$ADDRESS2_BASE32" >&2
  echo "EVM refs: $ADDRESS1_EVM_HEX $ADDRESS2_EVM_HEX" >&2
}

ensure_native_address() {
  local raw
  raw="$(strip_0x "$1")"
  local first="${raw:0:1}"
  first="${first,,}"
  case "$first" in
    0|1|8) return 0 ;;
  esac
  echo "Invalid native address: $1" >&2
  echo "Native addresses must start with 0x0, 0x1, or 0x8." >&2
  echo "Native defaults: $ADDRESS1_BASE32 $ADDRESS2_BASE32" >&2
  echo "EVM refs: $ADDRESS1_EVM_HEX $ADDRESS2_EVM_HEX" >&2
  exit 1
}

ensure_addrconv() {
  mkdir -p "$TOOLS_DIR"
  if [[ -x "$ADDRCONV_BIN" ]]; then
    return 0
  fi
  if ! command -v rustc >/dev/null 2>&1; then
    echo "rustc not found; cannot build addrconv helper." >&2
    return 1
  fi
  cargo build -p mazze-addr >/dev/null 2>&1
  local rlib
  rlib="$(ls "$REPO_ROOT/target/debug/deps/libmazze_addr-"*.rlib 2>/dev/null | head -n1)"
  if [[ -z "$rlib" ]]; then
    echo "mazze_addr library not found; run 'cargo build -p mazze-addr'." >&2
    return 1
  fi
  cat > "$TOOLS_DIR/addrconv.rs" <<'RS'
extern crate mazze_addr;

use mazze_addr::mazze_addr_decode;
use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: addrconv decode <value>");
        std::process::exit(1);
    }
    match args[1].as_str() {
        "decode" => match mazze_addr_decode(&args[2]) {
            Ok(decoded) => {
                if let Some(addr) = decoded.hex_address {
                    println!("0x{:x}", addr);
                } else {
                    eprintln!("decoded address is not SIZE_160");
                    std::process::exit(1);
                }
            }
            Err(err) => {
                eprintln!("{:?}", err);
                std::process::exit(1);
            }
        },
        _ => {
            eprintln!("usage: addrconv decode <value>");
            std::process::exit(1);
        }
    }
}
RS
  rustc "$TOOLS_DIR/addrconv.rs" \
    -L "$REPO_ROOT/target/debug/deps" \
    --extern mazze_addr="$rlib" \
    -o "$ADDRCONV_BIN"
  chmod +x "$ADDRCONV_BIN"
}

addr_to_hex() {
  local base32="$1"
  ensure_addrconv || return 1
  "$ADDRCONV_BIN" decode "$base32"
}

to_native_hex() {
  local input="$1"
  if [[ "$input" == 0x* || "$input" == 0X* ]]; then
    printf '%s' "$input"
    return 0
  fi
  if [[ "$input" == *:* || "$input" == mazze:* || "$input" == MAZZE:* ]]; then
    addr_to_hex "$input"
    return $?
  fi
  printf '%s' "$input"
}

case "$#" in
  0)
    usage
    exit 1
    ;;
  1)
    if is_amount "$1"; then
      AMOUNT_MAZZE="$1"
    else
      usage
      exit 1
    fi
    ;;
  2)
    if ! is_amount "$2"; then
      usage
      exit 1
    fi
    if is_privkey "$1"; then
      FROM_SECRET="$1"
      AMOUNT_MAZZE="$2"
    elif is_address "$1"; then
      TO_INPUT="$1"
      AMOUNT_MAZZE="$2"
    else
      usage
      exit 1
    fi
    ;;
  3)
    FROM_SECRET="$1"
    TO_INPUT="$2"
    AMOUNT_MAZZE="$3"
    ;;
  *)
    usage
    exit 1
    ;;
esac

TO_HEX="$(to_native_hex "$TO_INPUT")"
ensure_native_address "$TO_HEX"

VALUE_WEI="$(amount_to_wei "$AMOUNT_MAZZE")" || {
  echo "Invalid amount: $AMOUNT_MAZZE" >&2
  exit 1
}

MAZZEKEY_BIN="$REPO_ROOT/target/debug/mazzekey"
NATIVE_TX_BIN="$REPO_ROOT/target/debug/native_tx"

if [[ ! -x "$MAZZEKEY_BIN" ]]; then
  cargo build -p ctxkey-cli --bin mazzekey
fi

if [[ ! -x "$NATIVE_TX_BIN" ]]; then
  cargo build -p mazze-executor --bin native_tx
fi

rpc_call() {
  local method="$1"
  local params="$2"
  local response
  if ! response=$(curl -sS --fail -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${method}\",\"params\":${params}}" \
    "$RPC_URL"); then
    echo "RPC request failed: ${method} (RPC_URL=${RPC_URL})" >&2
    exit 1
  fi
  if [[ -z "$response" ]]; then
    echo "RPC response empty (RPC_URL=${RPC_URL}). Is the node running?" >&2
    exit 1
  fi
  printf '%s' "$response"
}

json_get() {
  local field="$1"
  py -c 'import json,sys
field=sys.argv[1]
raw=sys.stdin.read()
if not raw.strip():
    sys.stderr.write("Empty RPC response\n"); sys.exit(2)
try:
    data=json.loads(raw)
except Exception as exc:
    sys.stderr.write(f"Failed to parse JSON: {exc}\n{raw}\n"); sys.exit(2)
if "result" not in data:
    sys.stderr.write(f"RPC error: {data}\n"); sys.exit(2)
print(data["result"][field])' "$field"
}

json_result() {
  py -c 'import json,sys
raw=sys.stdin.read()
if not raw.strip():
    sys.stderr.write("Empty RPC response\n"); sys.exit(2)
try:
    data=json.loads(raw)
except Exception as exc:
    sys.stderr.write(f"Failed to parse JSON: {exc}\n{raw}\n"); sys.exit(2)
if "result" not in data:
    sys.stderr.write(f"RPC error: {data}\n"); sys.exit(2)
print(data["result"])'
}

rpc_call_result_retry() {
  local method="$1"
  local params="$2"
  local attempts="${3:-6}"
  local delay="${4:-0.25}"
  local response
  local result
  local i
  for ((i=1; i<=attempts; i++)); do
    response="$(rpc_call "$method" "$params")"
    if result="$(printf '%s' "$response" | json_result)"; then
      printf '%s' "$result"
      return 0
    fi
    if [[ "$response" == *'"code":-32016'* || "$response" == *"Locked"* || "$response" == *'"code":-32077'* || "$response" == *"catch up mode"* ]]; then
      sleep "$delay"
      continue
    fi
    return 1
  done
  return 1
}

status_json="$(rpc_call mazze_getStatus "[]")"
chain_id="$(printf '%s' "$status_json" | json_get chainId)"
epoch="$(printf '%s' "$status_json" | json_get epochNumber)"
network_id_hex="$(printf '%s' "$status_json" | json_get networkId)"
network_id="$(py -c 'import sys; print(int(sys.argv[1], 16))' "$network_id_hex")"

addr_info="$("$MAZZEKEY_BIN" info "$FROM_SECRET" --address --network "$network_id")"
from_base32="$(echo "$addr_info" | awk '/Base32 address:/ {print $3}')"
if [[ -z "$from_base32" ]]; then
  echo "Failed to derive base32 address from secret" >&2
  exit 1
fi

nonce="$(rpc_call_result_retry mazze_getNextNonce "[\"${from_base32}\",\"latest_state\"]")"

raw_tx="$("$NATIVE_TX_BIN" \
  --from-secret "$FROM_SECRET" \
  --to "$TO_HEX" \
  --value "$VALUE_WEI" \
  --gas "$GAS" \
  --gas-price "$GAS_PRICE" \
  --storage-limit "$STORAGE_LIMIT" \
  --nonce "$nonce" \
  --chain-id "$chain_id" \
  --epoch "$epoch")"

send_result="$(rpc_call_result_retry mazze_sendRawTransaction "[\"${raw_tx}\"]" 12 0.5)"
echo "$send_result"
