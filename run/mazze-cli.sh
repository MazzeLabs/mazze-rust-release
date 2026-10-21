#!/usr/bin/env bash
set -u
set -o pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

RPC_URL="${RPC_URL:-http://127.0.0.1:12539}"
NETWORK_ID="${MAZZE_NETWORK_ID:-1990}"
MAZZE_DECIMALS="${MAZZE_DECIMALS:-18}"
RPC_RETRIES="${RPC_RETRIES:-8}"
RPC_RETRY_DELAY="${RPC_RETRY_DELAY:-0.35}"

MAZZE_HOME="${MAZZE_HOME:-$HOME/.mazze}"
WALLET_DIR="$MAZZE_HOME/wallets"
TOOLS_DIR="$MAZZE_HOME/tools"
ADDRCONV_BIN="$TOOLS_DIR/addrconv"
DASHBOARD_ENABLED="${MAZZE_CLI_DASH:-1}"
DASHBOARD_REFRESH_SEC="${MAZZE_CLI_DASH_REFRESH:-2}"
DASHBOARD_LAST_TS=0
CLI_COLOR="${MAZZE_CLI_COLOR:-1}"
FAUCET_AMOUNT_MAZZE="${MAZZE_FAUCET_AMOUNT:-1000}"
if [[ -n "${NO_COLOR:-}" || ! -t 1 ]]; then
  CLI_COLOR="0"
fi

MAZZEKEY_BIN="$REPO_ROOT/target/debug/mazzekey"

color() {
  local code="$1"
  shift
  if [[ "$CLI_COLOR" == "1" ]]; then
    printf '\033[%sm%s\033[0m' "$code" "$*"
  else
    printf '%s' "$*"
  fi
}

dim() { color "2" "$*"; }
bold() { color "1" "$*"; }
green() { color "32" "$*"; }
yellow() { color "33" "$*"; }
blue() { color "34" "$*"; }

repeat_char() {
  local char="$1"
  local count="$2"
  printf '%*s' "$count" '' | tr ' ' "$char"
}

py() {
  if command -v python3 >/dev/null 2>&1; then
    python3 "$@"
  else
    python "$@"
  fi
}

ensure_mazze_home() {
  mkdir -p "$WALLET_DIR"
  chmod 700 "$MAZZE_HOME" "$WALLET_DIR" 2>/dev/null || true
}

ensure_addrconv() {
  ensure_mazze_home
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

use mazze_addr::{mazze_addr_decode, mazze_addr_encode, EncodingOptions, Network};
use std::env;

fn to_network(id: u64) -> Network {
    match id {
        1990 => Network::Main,
        1 => Network::Test,
        other => Network::Id(other),
    }
}

fn hex_to_bytes20(hex: &str) -> Result<[u8; 20], String> {
    let hex = hex.trim_start_matches("0x").trim_start_matches("0X");
    if hex.len() != 40 {
        return Err("hex address must be 20 bytes".to_string());
    }
    let mut out = [0u8; 20];
    for i in 0..20 {
        let idx = i * 2;
        let part = &hex[idx..idx + 2];
        out[i] = u8::from_str_radix(part, 16)
            .map_err(|_| "invalid hex".to_string())?;
    }
    Ok(out)
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: addrconv encode|decode <value> [network_id]");
        std::process::exit(1);
    }
    match args[1].as_str() {
        "encode" => {
            let bytes = match hex_to_bytes20(&args[2]) {
                Ok(bytes) => bytes,
                Err(err) => {
                    eprintln!("{}", err);
                    std::process::exit(1);
                }
            };
            let network_id = args
                .get(3)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(1990);
            let network = to_network(network_id);
            match mazze_addr_encode(&bytes, network, EncodingOptions::QrCode) {
                Ok(addr) => println!("{}", addr),
                Err(err) => {
                    eprintln!("{:?}", err);
                    std::process::exit(1);
                }
            }
        }
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
            eprintln!("usage: addrconv encode|decode <value> [network_id]");
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

addr_to_base32() {
  local hex="$1"
  ensure_addrconv || return 1
  "$ADDRCONV_BIN" encode "$hex" "$NETWORK_ID"
}

addr_to_hex() {
  local base32="$1"
  ensure_addrconv || return 1
  "$ADDRCONV_BIN" decode "$base32"
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

json_result_raw() {
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
import json as _json
print(_json.dumps(data["result"]))'
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
value=data["result"]
for part in field.split("."):
    value=value[part]
print(value)' "$field"
}

json_get_quiet() {
  local field="$1"
  py -c 'import json,sys
field=sys.argv[1]
raw=sys.stdin.read()
try:
    data=json.loads(raw)
except Exception:
    sys.exit(1)
if "result" not in data:
    sys.exit(1)
try:
    value=data["result"]
    for part in field.split("."):
        value=value[part]
    print(value)
except Exception:
    sys.exit(1)' "$field" 2>/dev/null || true
}

json_pretty() {
  py -c 'import json,sys
raw=sys.stdin.read()
try:
    data=json.loads(raw)
except Exception:
    print(raw); sys.exit(0)
print(json.dumps(data, indent=2, sort_keys=True))'
}

rpc_is_retryable() {
  local response="$1"
  if [[ "$response" == *'"code":-32016'* || "$response" == *'"code":-32077'* || "$response" == *"Locked"* || "$response" == *"catch up mode"* ]]; then
    return 0
  fi
  return 1
}

rpc_call() {
  local method="$1"
  local params="$2"
  local response
  if ! response=$(curl -sS --fail -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${method}\",\"params\":${params}}" \
    "$RPC_URL"); then
    echo "RPC request failed: ${method} (RPC_URL=${RPC_URL})" >&2
    return 1
  fi
  if [[ -z "$response" ]]; then
    echo "RPC response empty (RPC_URL=${RPC_URL}). Is the node running?" >&2
    return 1
  fi
  printf '%s' "$response"
}

rpc_call_with_retry() {
  local method="$1"
  local params="$2"
  local attempts="${3:-$RPC_RETRIES}"
  local delay="${4:-$RPC_RETRY_DELAY}"
  local response=""
  local i

  for ((i=1; i<=attempts; i++)); do
    if response="$(rpc_call "$method" "$params")"; then
      if rpc_is_retryable "$response"; then
        sleep "$delay"
        continue
      fi
      printf '%s' "$response"
      return 0
    fi
    sleep "$delay"
  done
  [[ -n "$response" ]] && printf '%s' "$response"
  return 1
}

rpc_call_result() {
  rpc_call_with_retry "$1" "$2" | json_result
}

rpc_call_pretty() {
  rpc_call_with_retry "$1" "$2" | json_pretty
}

rpc_call_result_raw() {
  rpc_call_with_retry "$1" "$2" | json_result_raw
}

ensure_mazzekey() {
  if [[ ! -x "$MAZZEKEY_BIN" ]]; then
    cargo build -p ctxkey-cli --bin mazzekey
  fi
}

wallet_dir() {
  printf '%s' "$WALLET_DIR/$1"
}

wallet_exists() {
  [[ -d "$(wallet_dir "$1")" ]]
}

is_wallet_name() {
  [[ "$1" =~ ^[A-Za-z0-9._-]+$ ]]
}

wallet_list_names() {
  if [[ -d "$WALLET_DIR" ]]; then
    ls -1 "$WALLET_DIR" 2>/dev/null || true
  fi
}

wallet_get_address() {
  local dir
  dir="$(wallet_dir "$1")"
  cat "$dir/address" 2>/dev/null || true
}

wallet_get_address_hex() {
  local dir
  dir="$(wallet_dir "$1")"
  cat "$dir/address_hex" 2>/dev/null || true
}

wallet_is_encrypted() {
  local dir
  dir="$(wallet_dir "$1")"
  [[ -f "$dir/secret.enc" ]]
}

prompt_secret() {
  local label="$1"
  local pass
  read -r -s -p "$label: " pass
  echo
  printf '%s' "$pass"
}

encrypt_secret() {
  local secret="$1"
  local out_file="$2"
  local pass="$3"
  if ! command -v openssl >/dev/null 2>&1; then
    echo "openssl not found; cannot encrypt wallet. Install openssl or create without password." >&2
    return 1
  fi
  printf '%s' "$secret" | \
    env MAZZE_WALLET_PASS="$pass" \
    openssl enc -aes-256-cbc -pbkdf2 -salt -a -pass env:MAZZE_WALLET_PASS > "$out_file"
}

decrypt_secret() {
  local in_file="$1"
  local pass="$2"
  if ! command -v openssl >/dev/null 2>&1; then
    echo "openssl not found; cannot decrypt wallet." >&2
    return 1
  fi
  env MAZZE_WALLET_PASS="$pass" \
    openssl enc -d -aes-256-cbc -pbkdf2 -a -pass env:MAZZE_WALLET_PASS -in "$in_file"
}

wallet_get_secret() {
  local name="$1"
  local dir
  dir="$(wallet_dir "$name")"
  if [[ -f "$dir/secret" ]]; then
    cat "$dir/secret"
    return 0
  fi
  if [[ -f "$dir/secret.enc" ]]; then
    local pass
    pass="${MAZZE_WALLET_PASS:-}"
    if [[ -z "$pass" ]]; then
      pass="$(prompt_secret "Wallet password")"
    fi
    decrypt_secret "$dir/secret.enc" "$pass"
    return $?
  fi
  echo "Wallet secret not found for $name" >&2
  return 1
}

resolve_wallet_address() {
  local input="$1"
  if [[ -z "$input" ]]; then
    return 1
  fi
  if wallet_exists "$input"; then
    wallet_get_address "$input"
    return
  fi
  if [[ "$input" == 0x* || "$input" == 0X* ]]; then
    addr_to_base32 "$input"
    return
  fi
  printf '%s' "$input"
}

resolve_wallet_address_hex() {
  local input="$1"
  if wallet_exists "$input"; then
    wallet_get_address_hex "$input"
    return
  fi
  if [[ "$input" == 0x* || "$input" == 0X* ]]; then
    printf '%s' "$input"
    return
  fi
  if [[ "$input" == *:* ]]; then
    addr_to_hex "$input"
    return
  fi
  printf '%s' "$input"
}

resolve_wallet_secret() {
  local input="$1"
  if wallet_exists "$input"; then
    wallet_get_secret "$input"
    return $?
  fi
  printf '%s' "$input"
}

require_base32() {
  local value="$1"
  if [[ "$value" == 0x* || "$value" == 0X* ]]; then
    echo "Expected base32 address; hex is not accepted by RPC." >&2
    echo "Tip: use 'wallet info <secret-hex>' to get base32." >&2
    return 1
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

print_balance() {
  local value_hex="$1"
  py -c 'import sys,decimal
value=sys.argv[1]
value=value.lower()
if value.startswith("0x"):
    v=int(value,16)
else:
    v=int(value)
decimals=int(sys.argv[2])
decimal.getcontext().prec = max(80, len(str(v)) + decimals + 5)
q=decimal.Decimal(v) / (decimal.Decimal(10) ** decimals)
def fmt(val):
    s=f"{val:f}"
    if "." in s:
        s=s.rstrip("0").rstrip(".")
    return s or "0"
print(f"hex: {value}")
print(f"wei: {v}")
print(f"mazze: {fmt(q)}")' "$value_hex" "$MAZZE_DECIMALS"
}

format_mazze_value() {
  local value_hex="$1"
  py -c 'import sys,decimal
value=sys.argv[1]
value=value.lower()
if value.startswith("0x"):
    v=int(value,16)
else:
    v=int(value)
decimals=int(sys.argv[2])
decimal.getcontext().prec = max(80, len(str(v)) + decimals + 5)
q=decimal.Decimal(v) / (decimal.Decimal(10) ** decimals)
def fmt(val):
    s=f"{val:f}"
    if "." in s:
        s=s.rstrip("0").rstrip(".")
    return s or "0"
print(fmt(q))' "$value_hex" "$MAZZE_DECIMALS"
}

hex_to_dec() {
  local value="$1"
  py -c 'import sys
v=sys.argv[1].strip()
if v.startswith("0x") or v.startswith("0X"):
    print(int(v,16))
else:
    print(int(v))' "$value"
}

dashboard_print() {
  local status
  status="$(rpc_call_with_retry mazze_getStatus "[]" 1 0.1 2>/dev/null)" || return
  local epoch block pending best
  epoch="$(printf '%s' "$status" | json_get_quiet progress.bestEpochNumber)"
  [[ -z "$epoch" ]] && epoch="$(printf '%s' "$status" | json_get_quiet epochNumber)"
  block="$(printf '%s' "$status" | json_get_quiet progress.bestBlockNumber)"
  [[ -z "$block" ]] && block="$(printf '%s' "$status" | json_get_quiet blockNumber)"
  pending="$(printf '%s' "$status" | json_get_quiet pendingTxNumber)"
  best="$(printf '%s' "$status" | json_get_quiet bestHash)"
  if [[ -z "$epoch" || -z "$block" || -z "$best" ]]; then
    return
  fi
  local epoch_dec block_dec pending_dec
  epoch_dec="$(hex_to_dec "$epoch")"
  block_dec="$(hex_to_dec "$block")"
  pending_dec="$(hex_to_dec "${pending:-0x0}")"
  local best_short
  best_short="${best:0:10}...${best: -6}"
  local line
  line="$(dim epoch) $(bold "$epoch_dec")  $(dim block) $(bold "$block_dec")  $(dim pending) $(bold "$pending_dec")  $(dim best) $(blue "$best_short")"
  echo "$line"
}

dashboard_maybe_print() {
  if [[ "$DASHBOARD_ENABLED" != "1" ]]; then
    return
  fi
  local now
  now="$(date +%s)"
  if (( now - DASHBOARD_LAST_TS < DASHBOARD_REFRESH_SEC )); then
    return
  fi
  DASHBOARD_LAST_TS="$now"
  dashboard_print
}

prompt() {
  local label="$1"
  local default="${2:-}"
  local input=""
  if [[ -n "$default" ]]; then
    read -r -p "${label} [${default}]: " input
  else
    read -r -p "${label}: " input
  fi
  if [[ -z "$input" ]]; then
    input="$default"
  fi
  printf '%s' "$input"
}

print_help() {
  cat <<EOF
$(bold "Mazze CLI (dev)")

$(bold "Usage:")
  $0 <command> [args]

$(bold "Wallet:")
  wallet new <name> [--password]
  wallet import <name> <secret-hex> [--password]
  wallet list [--no-balances]
  wallet balance --name <name> [--epoch <epoch>]
  wallet transfer --name <name> --dest <base32|wallet> --amount <value>
  wallet show <name>
  wallet address --name <name>
  wallet delete <name>

$(bold "Chain:")
  status
  summary
  balance <base32|wallet> [epoch]
  account <base32|wallet> [epoch]
  nonce <base32|wallet> [epoch]
  pending <base32|wallet>
  pending-txs <base32|wallet> [limit]
  tx <hash>
  receipt <hash>
  tx-status <hash>
  watch <hash> [interval] [timeout]
  addr <value>
  dashboard <on|off|status>
  wait [seconds]

$(bold "Transfers:")
  send <from-secret|wallet> <to-base32|wallet> <amount-mazze>
  faucet --name <wallet> [--amount <value>]

EOF
}

cmd_status() {
  rpc_call_pretty "mazze_getStatus" "[]"
}

cmd_summary() {
  local status
  status="$(rpc_call_with_retry mazze_getStatus "[]" 1 0.1)" || return 1
  local epoch block processed pending best chain_id network_id latest_state latest_confirmed latest_checkpoint randomx_epoch era_number latest_snapshot snapshot_count
  epoch="$(printf '%s' "$status" | json_get_quiet progress.bestEpochNumber)"
  [[ -z "$epoch" ]] && epoch="$(printf '%s' "$status" | json_get_quiet epochNumber)"
  block="$(printf '%s' "$status" | json_get_quiet progress.bestBlockNumber)"
  [[ -z "$block" ]] && block="$(printf '%s' "$status" | json_get_quiet blockNumber)"
  processed="$(printf '%s' "$status" | json_get_quiet progress.processedBlockCount)"
  pending="$(printf '%s' "$status" | json_get_quiet pendingTxNumber)"
  best="$(printf '%s' "$status" | json_get_quiet bestHash)"
  chain_id="$(printf '%s' "$status" | json_get_quiet chainId)"
  network_id="$(printf '%s' "$status" | json_get_quiet networkId)"
  latest_state="$(printf '%s' "$status" | json_get_quiet progress.latestStateEpochNumber)"
  [[ -z "$latest_state" ]] && latest_state="$(printf '%s' "$status" | json_get_quiet latestState)"
  latest_confirmed="$(printf '%s' "$status" | json_get_quiet progress.latestConfirmedEpochNumber)"
  [[ -z "$latest_confirmed" ]] && latest_confirmed="$(printf '%s' "$status" | json_get_quiet latestConfirmed)"
  latest_checkpoint="$(printf '%s' "$status" | json_get_quiet progress.latestCheckpointEpochNumber)"
  [[ -z "$latest_checkpoint" ]] && latest_checkpoint="$(printf '%s' "$status" | json_get_quiet latestCheckpoint)"
  randomx_epoch="$(printf '%s' "$status" | json_get_quiet randomx.epochNumber)"
  latest_snapshot="$(printf '%s' "$status" | json_get_quiet snapshots.latestSnapshotEpochNumber)"
  snapshot_count="$(printf '%s' "$status" | json_get_quiet snapshots.availableSnapshotCount)"
  era_number="$(printf '%s' "$status" | json_get_quiet era.number)"

  printf '%-16s %s\n' "epoch" "$(bold "$(hex_to_dec "$epoch")")"
  printf '%-16s %s\n' "block" "$(bold "$(hex_to_dec "$block")")"
  if [[ -n "$processed" ]]; then
    printf '%-16s %s\n' "processed" "$(bold "$(hex_to_dec "$processed")")"
  fi
  printf '%-16s %s\n' "pending" "$(bold "$(hex_to_dec "${pending:-0x0}")")"
  printf '%-16s %s\n' "best" "$(blue "${best:0:10}...${best: -6}")"
  printf '%-16s %s\n' "chainId" "$(bold "$(hex_to_dec "$chain_id")")"
  printf '%-16s %s\n' "networkId" "$(bold "$(hex_to_dec "$network_id")")"
  printf '%-16s %s\n' "latestState" "$(bold "$(hex_to_dec "$latest_state")")"
  printf '%-16s %s\n' "latestConfirmed" "$(bold "$(hex_to_dec "$latest_confirmed")")"
  printf '%-16s %s\n' "latestCheckpoint" "$(bold "$(hex_to_dec "$latest_checkpoint")")"
  if [[ -n "$latest_snapshot" ]]; then
    printf '%-16s %s\n' "latestSnapshot" "$(bold "$(hex_to_dec "$latest_snapshot")")"
  fi
  if [[ -n "$snapshot_count" ]]; then
    printf '%-16s %s\n' "snapshotCount" "$(bold "$(hex_to_dec "$snapshot_count")")"
  fi
  if [[ -n "$randomx_epoch" ]]; then
    printf '%-16s %s\n' "randomxEpoch" "$(bold "$(hex_to_dec "$randomx_epoch")")"
  fi
  if [[ -n "$era_number" ]]; then
    printf '%-16s %s\n' "era" "$(bold "$(hex_to_dec "$era_number")")"
  fi
}

cmd_wallet_new() {
  local name="${1:-}"
  local with_password="${2:-}"
  if [[ -z "$name" ]]; then
    echo "Usage: wallet new <name> [--password]" >&2
    return 1
  fi
  if ! is_wallet_name "$name"; then
    echo "Invalid wallet name. Use letters, numbers, dot, dash, underscore." >&2
    return 1
  fi
  ensure_mazze_home
  if wallet_exists "$name"; then
    echo "Wallet already exists: $name" >&2
    return 1
  fi
  ensure_mazzekey
  local secret
  secret="$(py -c 'import secrets; print(secrets.token_hex(32))')"
  local info hex_addr base32_addr
  info="$("$MAZZEKEY_BIN" info "$secret" --address --network "$NETWORK_ID")"
  hex_addr="$(printf '%s' "$info" | awk -F': ' '/Hex address:/ {print $2}')"
  base32_addr="$(printf '%s' "$info" | awk -F': ' '/Base32 address:/ {print $2}')"
  if [[ -z "$base32_addr" || -z "$hex_addr" ]]; then
    echo "Failed to derive address for new wallet." >&2
    return 1
  fi
  hex_addr="${hex_addr#0x}"
  hex_addr="0x${hex_addr}"
  local dir
  dir="$(wallet_dir "$name")"
  mkdir -p "$dir"
  chmod 700 "$dir" 2>/dev/null || true
  printf '%s' "$base32_addr" > "$dir/address"
  printf '%s' "$hex_addr" > "$dir/address_hex"
  date -u +"%Y-%m-%dT%H:%M:%SZ" > "$dir/created"

  if [[ "$with_password" == "--password" || "$with_password" == "-p" ]]; then
    local pass1 pass2
    pass1="$(prompt_secret "Set wallet password")"
    pass2="$(prompt_secret "Confirm password")"
    if [[ -z "$pass1" || "$pass1" != "$pass2" ]]; then
      echo "Password mismatch." >&2
      rm -rf "$dir"
      return 1
    fi
    if ! encrypt_secret "$secret" "$dir/secret.enc" "$pass1"; then
      rm -rf "$dir"
      return 1
    fi
  else
    printf '%s' "$secret" > "$dir/secret"
    chmod 600 "$dir/secret" 2>/dev/null || true
  fi

  echo "Wallet created: $name"
  echo "Address: $base32_addr"
}

cmd_wallet_info() {
  local name="${1:-}"
  if [[ -z "$name" ]]; then
    echo "Usage: wallet show <name>" >&2
    return 1
  fi
  if ! wallet_exists "$name"; then
    echo "Wallet not found: $name" >&2
    return 1
  fi
  local dir
  dir="$(wallet_dir "$name")"
  local address hex
  address="$(wallet_get_address "$name")"
  hex="$(wallet_get_address_hex "$name")"
  echo "name: $name"
  echo "address: $address"
  echo "hex: $hex"
  if wallet_is_encrypted "$name"; then
    echo "encrypted: true"
  else
    echo "encrypted: false"
  fi
  if [[ -f "$dir/created" ]]; then
    echo "created: $(cat "$dir/created")"
  fi
}

cmd_wallet_address() {
  local name=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --name|-n)
        name="${2:-}"
        shift 2
        ;;
      *)
        if [[ -z "$name" ]]; then
          name="$1"
        else
          echo "Unknown argument: $1" >&2
          return 1
        fi
        shift
        ;;
    esac
  done
  if [[ -z "$name" ]]; then
    echo "Usage: wallet address <name>" >&2
    return 1
  fi
  if ! wallet_exists "$name"; then
    echo "Wallet not found: $name" >&2
    return 1
  fi
  wallet_get_address "$name"
}

cmd_wallet_import() {
  local name="${1:-}"
  local secret="${2:-}"
  local with_password="${3:-}"
  if [[ -z "$name" || -z "$secret" ]]; then
    echo "Usage: wallet import <name> <secret-hex> [--password]" >&2
    return 1
  fi
  if ! is_wallet_name "$name"; then
    echo "Invalid wallet name. Use letters, numbers, dot, dash, underscore." >&2
    return 1
  fi
  ensure_mazze_home
  if wallet_exists "$name"; then
    echo "Wallet already exists: $name" >&2
    return 1
  fi
  ensure_mazzekey
  local info hex_addr base32_addr
  info="$("$MAZZEKEY_BIN" info "$secret" --address --network "$NETWORK_ID")"
  hex_addr="$(printf '%s' "$info" | awk -F': ' '/Hex address:/ {print $2}')"
  base32_addr="$(printf '%s' "$info" | awk -F': ' '/Base32 address:/ {print $2}')"
  if [[ -z "$base32_addr" || -z "$hex_addr" ]]; then
    echo "Failed to derive address for wallet import." >&2
    return 1
  fi
  hex_addr="${hex_addr#0x}"
  hex_addr="0x${hex_addr}"
  local dir
  dir="$(wallet_dir "$name")"
  mkdir -p "$dir"
  chmod 700 "$dir" 2>/dev/null || true
  printf '%s' "$base32_addr" > "$dir/address"
  printf '%s' "$hex_addr" > "$dir/address_hex"
  date -u +"%Y-%m-%dT%H:%M:%SZ" > "$dir/created"

  if [[ "$with_password" == "--password" || "$with_password" == "-p" ]]; then
    local pass1 pass2
    pass1="$(prompt_secret "Set wallet password")"
    pass2="$(prompt_secret "Confirm password")"
    if [[ -z "$pass1" || "$pass1" != "$pass2" ]]; then
      echo "Password mismatch." >&2
      rm -rf "$dir"
      return 1
    fi
    if ! encrypt_secret "$secret" "$dir/secret.enc" "$pass1"; then
      rm -rf "$dir"
      return 1
    fi
  else
    printf '%s' "$secret" > "$dir/secret"
    chmod 600 "$dir/secret" 2>/dev/null || true
  fi

  echo "Wallet imported: $name"
  echo "Address: $base32_addr"
}

cmd_wallet_list() {
  local with_balances="true"
  case "${1:-}" in
    --no-balances|--no-balance|--lite)
      with_balances="false"
      ;;
    --balances)
      with_balances="true"
      ;;
  esac
  ensure_mazze_home
  local name_w=20
  local addr_w=60
  local bal_w=18
  local header_line sep_line
  header_line="$(printf '%-*s  %-*s' "$name_w" "name" "$addr_w" "address")"
  if [[ "$with_balances" == "true" ]]; then
    header_line+="$(printf '  %-*s' "$bal_w" "balance")"
  fi
  sep_line="$(printf '%-*s  %-*s' "$name_w" "$(repeat_char '-' "$name_w")" "$addr_w" "$(repeat_char '-' "$addr_w")")"
  if [[ "$with_balances" == "true" ]]; then
    sep_line+="$(printf '  %-*s' "$bal_w" "$(repeat_char '-' "$bal_w")")"
  fi
  printf '%s\n' "$(bold "$header_line")"
  printf '%s\n' "$(dim "$sep_line")"
  local name
  for name in $(wallet_list_names); do
    local addr
    addr="$(wallet_get_address "$name")"
    printf '%-*s  %-*s' "$name_w" "$name" "$addr_w" "$addr"
    if [[ "$with_balances" == "true" ]]; then
      local bal_raw
      bal_raw="$(rpc_call_result mazze_getBalance "[\"${addr}\",\"latest_state\"]" 2>/dev/null || true)"
      if [[ -n "$bal_raw" ]]; then
        printf '  %-*s' "$bal_w" "$(format_mazze_value "$bal_raw")"
      else
        printf '  %-*s' "$bal_w" "n/a"
      fi
    fi
    printf '\n'
  done
}

cmd_wallet_balance() {
  local name=""
  local epoch="latest_state"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --name|-n)
        name="${2:-}"
        shift 2
        ;;
      --epoch|-e)
        epoch="${2:-latest_state}"
        shift 2
        ;;
      *)
        if [[ -z "$name" ]]; then
          name="$1"
        else
          epoch="$1"
        fi
        shift
        ;;
    esac
  done
  if [[ -z "$name" ]]; then
    echo "Usage: wallet balance --name <name> [--epoch <epoch>]" >&2
    return 1
  fi
  if ! wallet_exists "$name"; then
    echo "Wallet not found: $name" >&2
    return 1
  fi
  local addr bal_raw bal_fmt
  addr="$(wallet_get_address "$name")"
  bal_raw="$(rpc_call_result mazze_getBalance "[\"${addr}\",\"${epoch}\"]")" || return 1
  bal_fmt="$(format_mazze_value "$bal_raw")"
  printf '%s\n' "$(dim "balance:") $(bold "${bal_fmt}") MAZZE"
}

cmd_wallet_transfer() {
  local name=""
  local dest=""
  local amount=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --name|-n)
        name="${2:-}"
        shift 2
        ;;
      --dest|--to)
        dest="${2:-}"
        shift 2
        ;;
      dest)
        dest="${2:-}"
        shift 2
        ;;
      --amount|-a)
        amount="${2:-}"
        shift 2
        ;;
      *)
        if [[ -z "$dest" ]]; then
          dest="$1"
        elif [[ -z "$amount" ]]; then
          amount="$1"
        else
          echo "Unknown argument: $1" >&2
          return 1
        fi
        shift
        ;;
    esac
  done
  if [[ -z "$name" || -z "$dest" || -z "$amount" ]]; then
    echo "Usage: wallet transfer --name <name> --dest <base32|wallet> --amount <value>" >&2
    return 1
  fi
  if ! wallet_exists "$name"; then
    echo "Wallet not found: $name" >&2
    return 1
  fi
  if ! amount_to_wei "$amount" >/dev/null; then
    echo "Invalid amount: $amount" >&2
    return 1
  fi

  local secret to_addr
  secret="$(resolve_wallet_secret "$name")" || return 1
  to_addr="$(resolve_wallet_address "$dest")"
  if [[ -z "$to_addr" ]]; then
    echo "Invalid destination: $dest" >&2
    return 1
  fi
  require_base32 "$to_addr" || return 1
  "$SCRIPT_DIR/send-transfer.sh" "$secret" "$to_addr" "$amount"
}

cmd_wallet_delete() {
  local name="${1:-}"
  if [[ -z "$name" ]]; then
    echo "Usage: wallet delete <name>" >&2
    return 1
  fi
  if ! wallet_exists "$name"; then
    echo "Wallet not found: $name" >&2
    return 1
  fi
  rm -rf "$(wallet_dir "$name")"
  echo "Wallet deleted: $name"
}

cmd_balance() {
  local input="${1:-}"
  local epoch="${2:-latest_state}"
  local addr
  addr="$(resolve_wallet_address "$input")"
  if [[ -z "$addr" ]]; then
    echo "Usage: balance <wallet-name|base32> [epoch]" >&2
    return 1
  fi
  require_base32 "$addr" || return 1
  local result
  result="$(rpc_call_result mazze_getBalance "[\"${addr}\",\"${epoch}\"]")" || return 1
  print_balance "$result"
}

cmd_account() {
  local input="${1:-}"
  local epoch="${2:-latest_state}"
  local addr
  addr="$(resolve_wallet_address "$input")"
  if [[ -z "$addr" ]]; then
    echo "Usage: account <wallet-name|base32> [epoch]" >&2
    return 1
  fi
  require_base32 "$addr" || return 1
  rpc_call_pretty "mazze_getAccount" "[\"${addr}\",\"${epoch}\"]"
}

cmd_nonce() {
  local input="${1:-}"
  local epoch="${2:-latest_state}"
  local addr
  addr="$(resolve_wallet_address "$input")"
  if [[ -z "$addr" ]]; then
    echo "Usage: nonce <wallet-name|base32> [epoch]" >&2
    return 1
  fi
  require_base32 "$addr" || return 1
  rpc_call_result mazze_getNextNonce "[\"${addr}\",\"${epoch}\"]"
}

cmd_pending() {
  local input="${1:-}"
  local addr
  addr="$(resolve_wallet_address "$input")"
  if [[ -z "$addr" ]]; then
    echo "Usage: pending <wallet-name|base32>" >&2
    return 1
  fi
  require_base32 "$addr" || return 1
  rpc_call_pretty "mazze_getAccountPendingInfo" "[\"${addr}\"]"
}

cmd_pending_txs() {
  local input="${1:-}"
  local limit="${2:-10}"
  local addr
  addr="$(resolve_wallet_address "$input")"
  if [[ -z "$addr" ]]; then
    echo "Usage: pending-txs <wallet-name|base32> [limit]" >&2
    return 1
  fi
  require_base32 "$addr" || return 1
  rpc_call_pretty "mazze_getAccountPendingTransactions" "[\"${addr}\",null,${limit}]"
}

cmd_tx() {
  local hash="${1:-}"
  if [[ -z "$hash" ]]; then
    echo "Usage: tx <hash>" >&2
    return 1
  fi
  local result
  result="$(rpc_call_result_raw mazze_getTransactionByHash "[\"${hash}\"]")" || return 1
  if [[ "$result" == "null" ]]; then
    local receipt
    receipt="$(rpc_call_result_raw mazze_getTransactionReceipt "[\"${hash}\"]")" || return 1
    if [[ "$receipt" == "null" ]]; then
      echo "null"
      return 0
    fi
    printf '%s' "$receipt" | py -c 'import json, sys, decimal

decimals = int(sys.argv[1])
raw = sys.stdin.read()
if not raw.strip():
    print("null")
    sys.exit(0)
receipt = json.loads(raw)
if receipt is None:
    print("null")
    sys.exit(0)

def hex_to_int(val):
    if isinstance(val, str) and val.startswith(("0x", "0X")):
        try:
            return int(val, 16)
        except Exception:
            return val
    return val

def normalize_addr(addr):
    if not isinstance(addr, str):
        return addr, None
    if ":" not in addr:
        return addr, None
    lower = addr.lower()
    parts = lower.split(":")
    prefix = parts[0]
    payload = parts[-1]
    addr_type = None
    for part in parts[1:-1]:
        if part.startswith("type."):
            addr_type = part.split(".", 1)[1]
            break
    return f"{prefix}:{payload}", addr_type

for key, type_key in (("from", "fromType"), ("to", "toType"), ("contractCreated", "contractCreatedType")):
    if key in receipt and receipt[key] is not None:
        simple, addr_type = normalize_addr(receipt[key])
        receipt[key] = simple
        if addr_type:
            receipt[type_key] = addr_type

for field in (
    "epochNumber",
    "index",
    "gasUsed",
    "accumulatedGasUsed",
    "gasFee",
    "effectiveGasPrice",
    "outcomeStatus",
    "storageCollateralized",
    "burntGasFee",
):
    if field in receipt:
        receipt[field] = hex_to_int(receipt[field])

print(json.dumps(receipt, indent=2, sort_keys=True))' "$MAZZE_DECIMALS"
    return 0
  fi
  printf '%s' "$result" | py -c 'import json, sys, decimal

decimals = int(sys.argv[1])
raw = sys.stdin.read()
if not raw.strip():
    print("null")
    sys.exit(0)
tx = json.loads(raw)
if tx is None:
    print("null")
    sys.exit(0)

def hex_to_int(val):
    if isinstance(val, str) and val.startswith(("0x", "0X")):
        try:
            return int(val, 16)
        except Exception:
            return val
    return val

def format_mazze(wei):
    decimal.getcontext().prec = max(50, len(str(wei)) + decimals)
    q = decimal.Decimal(wei) / (decimal.Decimal(10) ** decimals)
    s = f"{q:f}"
    if "." in s:
        s = s.rstrip("0").rstrip(".")
    return s or "0"

def normalize_addr(addr):
    if not isinstance(addr, str):
        return addr, None
    if ":" not in addr:
        return addr, None
    lower = addr.lower()
    parts = lower.split(":")
    prefix = parts[0]
    payload = parts[-1]
    addr_type = None
    for part in parts[1:-1]:
        if part.startswith("type."):
            addr_type = part.split(".", 1)[1]
            break
    return f"{prefix}:{payload}", addr_type

from_addr = tx.get("from")
to_addr = tx.get("to")

from_simple, from_type = normalize_addr(from_addr)
to_simple, to_type = normalize_addr(to_addr)

if from_simple:
    tx["from"] = from_simple
if from_type:
    tx["fromType"] = from_type
if to_simple:
    tx["to"] = to_simple
if to_type:
    tx["toType"] = to_type

for field in (
    "chainId",
    "epochHeight",
    "gas",
    "gasPrice",
    "nonce",
    "storageLimit",
    "transactionIndex",
    "type",
    "status",
    "v",
):
    if field in tx:
        tx[field] = hex_to_int(tx[field])

if "value" in tx:
    raw_val = hex_to_int(tx["value"])
    if isinstance(raw_val, int):
        tx["value"] = format_mazze(raw_val)
    else:
        tx["value"] = raw_val

print(json.dumps(tx, indent=2, sort_keys=True))' "$MAZZE_DECIMALS"
}

cmd_receipt() {
  local hash="${1:-}"
  if [[ -z "$hash" ]]; then
    echo "Usage: receipt <hash>" >&2
    return 1
  fi
  rpc_call_pretty "mazze_getTransactionReceipt" "[\"${hash}\"]"
}

cmd_tx_status() {
  local hash="${1:-}"
  if [[ -z "$hash" ]]; then
    echo "Usage: tx-status <hash>" >&2
    return 1
  fi
  local result
  result="$(rpc_call_result_raw mazze_getTransactionReceipt "[\"${hash}\"]")" || return 1
  if [[ "$result" == "null" ]]; then
    echo "status: pending"
    return 0
  fi
  printf '%s' "$result" | json_pretty
}

cmd_watch() {
  local hash="${1:-}"
  local interval="${2:-2}"
  local timeout="${3:-120}"
  if [[ -z "$hash" ]]; then
    echo "Usage: watch <hash> [interval] [timeout]" >&2
    return 1
  fi
  local start
  start="$(date +%s)"
  while true; do
    local result
    result="$(rpc_call_result_raw mazze_getTransactionReceipt "[\"${hash}\"]" 2>/dev/null || true)"
    if [[ -n "$result" && "$result" != "null" ]]; then
      printf '%s' "$result" | json_pretty
      return 0
    fi
    local now elapsed
    now="$(date +%s)"
    elapsed=$((now - start))
    if (( elapsed >= timeout )); then
      echo "Timeout waiting for receipt." >&2
      return 1
    fi
    echo "pending... (${elapsed}s)"
    sleep "$interval"
  done
}

cmd_addr() {
  local value="${1:-}"
  if [[ -z "$value" ]]; then
    echo "Usage: addr <value>" >&2
    return 1
  fi
  if [[ "$value" == 0x* || "$value" == 0X* ]]; then
    local base32
    base32="$(addr_to_base32 "$value")" || return 1
    echo "hex: $value"
    echo "base32: $base32"
    return 0
  fi
  local hex
  hex="$(addr_to_hex "$value")" || return 1
  echo "base32: $value"
  echo "hex: $hex"
}

cmd_dashboard() {
  local action="${1:-status}"
  case "$action" in
    on)
      DASHBOARD_ENABLED="1"
      echo "dashboard: on"
      ;;
    off)
      DASHBOARD_ENABLED="0"
      echo "dashboard: off"
      ;;
    status)
      echo "dashboard: ${DASHBOARD_ENABLED}"
      ;;
    *)
      echo "Usage: dashboard <on|off|status>" >&2
      return 1
      ;;
  esac
}

cmd_send() {
  if [[ $# -eq 0 ]]; then
    local from_name to_input amount
    from_name="$(prompt "from wallet or secret" "")"
    to_input="$(prompt "to wallet or base32" "")"
    amount="$(prompt "amount (MAZZE)" "")"
    if [[ -z "$from_name" || -z "$to_input" || -z "$amount" ]]; then
      echo "From, destination, and amount are required." >&2
      return 1
    fi
    local from_secret
    from_secret="$(resolve_wallet_secret "$from_name")" || return 1
    local to_addr
    to_addr="$(resolve_wallet_address "$to_input")"
    if [[ -z "$to_addr" ]]; then
      echo "Invalid destination: $to_input" >&2
      return 1
    fi
    require_base32 "$to_addr" || return 1
    "$SCRIPT_DIR/send-transfer.sh" "$from_secret" "$to_addr" "$amount"
    return
  fi

  if [[ $# -lt 3 ]]; then
    echo "Usage: send <from-wallet|secret> <to-wallet|base32> <amount>" >&2
    return 1
  fi

  local args=("$@")
  if wallet_exists "${args[0]}"; then
    args[0]="$(resolve_wallet_secret "${args[0]}")"
  fi
  if [[ ${#args[@]} -ge 2 ]] && wallet_exists "${args[1]}"; then
    args[1]="$(resolve_wallet_address "${args[1]}")"
  fi
  if [[ ${#args[@]} -ge 2 ]]; then
    require_base32 "${args[1]}" || return 1
  fi
  "$SCRIPT_DIR/send-transfer.sh" "${args[@]}"
}

cmd_faucet() {
  local name=""
  local amount="$FAUCET_AMOUNT_MAZZE"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --name|-n)
        name="${2:-}"
        shift 2
        ;;
      --amount|-a)
        amount="${2:-$FAUCET_AMOUNT_MAZZE}"
        shift 2
        ;;
      *)
        if [[ -z "$name" ]]; then
          name="$1"
        else
          echo "Unknown argument: $1" >&2
          return 1
        fi
        shift
        ;;
    esac
  done
  if [[ -z "$name" ]]; then
    echo "Usage: faucet --name <wallet> [--amount <value>]" >&2
    return 1
  fi
  if ! wallet_exists "$name"; then
    echo "Wallet not found: $name" >&2
    return 1
  fi
  local dest
  dest="$(resolve_wallet_address "$name")"
  if [[ -z "$dest" ]]; then
    echo "Invalid wallet address for $name" >&2
    return 1
  fi
  require_base32 "$dest" || return 1
  "$SCRIPT_DIR/send-transfer.sh" "$dest" "$amount"
}

cmd_wait() {
  local seconds="${1:-10}"
  local end=$((SECONDS + seconds))
  while (( SECONDS < end )); do
    if rpc_call_with_retry "mazze_getStatus" "[]" 1 0.1 >/dev/null 2>&1; then
      echo "Node is responding."
      return 0
    fi
    sleep 0.5
  done
  echo "Timed out waiting for node." >&2
  return 1
}

run_command() {
  local cmd="${1:-help}"
  shift || true
  case "$cmd" in
    help|-h|--help) print_help ;;
    status) cmd_status ;;
    summary) cmd_summary ;;
    wallet)
      case "${1:-}" in
        new) shift; cmd_wallet_new "$@" ;;
        import) shift; cmd_wallet_import "$@" ;;
        list) shift; cmd_wallet_list "$@" ;;
        balance) shift; cmd_wallet_balance "$@" ;;
        transfer) shift; cmd_wallet_transfer "$@" ;;
        show|info) shift; cmd_wallet_info "$@" ;;
        address) shift; cmd_wallet_address "$@" ;;
        delete|rm) shift; cmd_wallet_delete "$@" ;;
        *) echo "Usage: wallet {new|import|list|balance|transfer|show|address|delete}" >&2; return 1 ;;
      esac
      ;;
    balance) cmd_balance "$@" ;;
    account) cmd_account "$@" ;;
    nonce) cmd_nonce "$@" ;;
    pending) cmd_pending "$@" ;;
    pending-txs) cmd_pending_txs "$@" ;;
    send) cmd_send "$@" ;;
    tx) cmd_tx "$@" ;;
    receipt) cmd_receipt "$@" ;;
    tx-status) cmd_tx_status "$@" ;;
    watch) cmd_watch "$@" ;;
    addr) cmd_addr "$@" ;;
    dashboard) cmd_dashboard "$@" ;;
    faucet) cmd_faucet "$@" ;;
    wait) cmd_wait "$@" ;;
    exit|quit) return 2 ;;
    *) echo "Unknown command: $cmd" >&2; return 1 ;;
  esac
}

if [[ $# -gt 0 ]]; then
  run_command "$@" || exit $?
  exit 0
fi

ensure_mazze_home

printf '%s\n' "$(bold "Mazze CLI") ready. Type 'help' for commands."
while true; do
  dashboard_maybe_print
  prompt_text="$(bold "mazze> ")"
  read -r -p "$prompt_text" line || break
  line="${line#"${line%%[![:space:]]*}"}"
  [[ -z "$line" ]] && continue
  read -r -a parts <<<"$line"
  run_command "${parts[@]}"
  status=$?
  if [[ $status -eq 2 ]]; then
    break
  fi
done
