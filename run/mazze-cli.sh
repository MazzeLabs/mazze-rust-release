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
SHIELDED_FROM_EPOCH="${MAZZE_SHIELDED_FROM_EPOCH:-0x0}"
SHIELDED_LOG_CHUNK="${MAZZE_SHIELDED_LOG_CHUNK:-500}"
CLI_COLOR="${MAZZE_CLI_COLOR:-1}"
FAUCET_AMOUNT_MAZZE="${MAZZE_FAUCET_AMOUNT:-1000}"
if [[ -n "${NO_COLOR:-}" || ! -t 1 ]]; then
  CLI_COLOR="0"
fi

SHIELDED_POOL_BASE32="MAZZE:TYPE.BUILTIN:AAEJUAAAAAAAAAAAAAAAAAAAAAAAAAAABAJ1SJV3W2"

MAZZEKEY_BIN="$REPO_ROOT/target/debug/mazzekey"
SHIELDED_NOTE_BIN="$REPO_ROOT/target/debug/shielded_note"

SELECTOR_ROOT="0xebf0c717"
SELECTOR_VK_HASH="0x69b5d6d1"
SELECTOR_NULLIFIER="0xd5a4e325"

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

ensure_shielded_note() {
  if [[ ! -x "$SHIELDED_NOTE_BIN" ]]; then
    cargo build -p mazze-executor --bin shielded_note
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

wallet_get_shielded_address() {
  local name="$1"
  local dir
  dir="$(wallet_dir "$name")"
  if [[ -f "$dir/shielded_address" ]]; then
    cat "$dir/shielded_address"
    return 0
  fi
  local secret
  secret="$(wallet_get_secret "$name")" || return 1
  ensure_shielded_note
  local addr
  addr="$("$SHIELDED_NOTE_BIN" address --secret "$secret" --network-id "$NETWORK_ID")" || return 1
  printf '%s' "$addr" > "$dir/shielded_address"
  echo "$addr"
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

resolve_wallet_shielded_address() {
  local input="$1"
  if wallet_exists "$input"; then
    wallet_get_shielded_address "$input"
    return
  fi
  if [[ "$input" == 0x* || "$input" == 0X* ]]; then
    ensure_shielded_note
    "$SHIELDED_NOTE_BIN" address --public "$input" --network-id "$NETWORK_ID"
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

pad32_hex() {
  local value="$1"
  value="${value#0x}"
  value="${value#0X}"
  if [[ ${#value} -gt 64 ]]; then
    return 1
  fi
  printf '%064s' "$value" | tr ' ' '0'
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

shielded_logs_pairs() {
  local from_epoch="${1:-$SHIELDED_FROM_EPOCH}"
  local from_dec to_dec
  from_dec="$(resolve_epoch_to_dec "$from_epoch")" || return 1
  to_dec="$(resolve_epoch_to_dec "latest_state")" || return 1
  local chunk="$SHIELDED_LOG_CHUNK"
  if ! [[ "$chunk" =~ ^[0-9]+$ ]] || (( chunk <= 0 )); then
    chunk=500
  fi
  local start="$from_dec"
  while (( start <= to_dec )); do
    local end=$((start + chunk - 1))
    if (( end > to_dec )); then
      end="$to_dec"
    fi
    local from_hex to_hex filter logs_json
    from_hex="$(dec_to_hex "$start")"
    to_hex="$(dec_to_hex "$end")"
    filter="$(printf '{"fromEpoch":"%s","toEpoch":"%s","address":"%s"}' "$from_hex" "$to_hex" "$SHIELDED_POOL_BASE32")"
    logs_json="$(rpc_call_result_raw mazze_getLogs "[$filter]")" || return 1
    if [[ -n "$logs_json" && "$logs_json" != "null" ]]; then
      printf '%s' "$logs_json" | py -c 'import json,sys
logs=json.loads(sys.stdin.read() or "[]")
for log in logs:
    topics = log.get("topics") or []
    if len(topics) < 2:
        continue
    commitment = topics[1]
    data_hex = (log.get("data") or "0x").lower()
    if not data_hex.startswith("0x"):
        continue
    data = bytes.fromhex(data_hex[2:])
    if not data:
        continue
    cipher = None
    if len(data) >= 32:
        head = int.from_bytes(data[0:32], "big")
        # ABI dynamic (offset+len) or compact (len+data)
        if head + 32 == len(data):
            cipher = data[32:]
        else:
            offset = head
            if offset + 32 <= len(data):
                length = int.from_bytes(data[offset:offset+32], "big")
                start = offset + 32
                end = start + length
                if end <= len(data):
                    cipher = data[start:end]
    if cipher is None:
        cipher = data
    if not cipher:
        continue
    print(f"{commitment} 0x{cipher.hex()}")'
    fi
    start=$((end + 1))
  done
}

nullifier_is_spent() {
  local nullifier="$1"
  local padded
  padded="$(pad32_hex "$nullifier")" || return 1
  local data="${SELECTOR_NULLIFIER}${padded}"
  local result
  result="$(rpc_call_result mazze_call "[{\"to\":\"${SHIELDED_POOL_BASE32}\",\"data\":\"${data}\"},\"latest_state\"]" 2>/dev/null)" || return 1
  local value
  value="$(py -c 'import sys
v=sys.argv[1].strip()
try:
    if v.startswith(("0x","0X")):
        print(int(v,16))
    else:
        print(int(v))
except Exception:
    sys.exit(1)' "$result")" || return 1
  if [[ "$value" == "0" ]]; then
    return 1
  fi
  return 0
}

shielded_notes_for_secret() {
  local secret="$1"
  local from_epoch="${2:-$SHIELDED_FROM_EPOCH}"
  ensure_shielded_note
  local pairs
  if ! pairs="$(shielded_logs_pairs "$from_epoch")"; then
    return 1
  fi
  if [[ -z "$pairs" ]]; then
    return 0
  fi
  while read -r commitment ciphertext; do
    if [[ -z "$commitment" || -z "$ciphertext" ]]; then
      continue
    fi
    local note_json
    note_json="$("$SHIELDED_NOTE_BIN" decrypt --secret "$secret" --commitment "$commitment" --ciphertext "$ciphertext" 2>/dev/null)" || continue
    local value nullifier
    value="$(printf '%s' "$note_json" | py -c 'import json,sys; print(json.load(sys.stdin)["value"])')" || continue
    nullifier="$(printf '%s' "$note_json" | py -c 'import json,sys; print(json.load(sys.stdin)["nullifier"])')" || continue
    if nullifier_is_spent "$nullifier"; then
      continue
    fi
    printf '%s %s\n' "$value" "$nullifier"
  done <<< "$pairs"
}

shielded_notes_full() {
  local secret="$1"
  local from_epoch="${2:-$SHIELDED_FROM_EPOCH}"
  ensure_shielded_note
  local pairs
  if ! pairs="$(shielded_logs_pairs "$from_epoch")"; then
    return 1
  fi
  if [[ -z "$pairs" ]]; then
    return 0
  fi
  local idx=0
  while read -r commitment ciphertext; do
    if [[ -z "$commitment" || -z "$ciphertext" ]]; then
      idx=$((idx + 1))
      continue
    fi
    local note_json
    note_json="$("$SHIELDED_NOTE_BIN" decrypt --secret "$secret" --commitment "$commitment" --ciphertext "$ciphertext" 2>/dev/null)" || { idx=$((idx + 1)); continue; }
    local value rho rseed nullifier
    value="$(printf '%s' "$note_json" | py -c 'import json,sys; print(json.load(sys.stdin)["value"])')" || { idx=$((idx + 1)); continue; }
    rho="$(printf '%s' "$note_json" | py -c 'import json,sys; print(json.load(sys.stdin)["rho"])')" || { idx=$((idx + 1)); continue; }
    rseed="$(printf '%s' "$note_json" | py -c 'import json,sys; print(json.load(sys.stdin)["rseed"])')" || { idx=$((idx + 1)); continue; }
    nullifier="$(printf '%s' "$note_json" | py -c 'import json,sys; print(json.load(sys.stdin)["nullifier"])')" || { idx=$((idx + 1)); continue; }
    if nullifier_is_spent "$nullifier"; then
      idx=$((idx + 1))
      continue
    fi
    printf '%s %s %s %s %s %s\n' "$idx" "$commitment" "$value" "$rho" "$rseed" "$nullifier"
    idx=$((idx + 1))
  done <<< "$pairs"
}

shielded_balance_for_secret() {
  local secret="$1"
  local from_epoch="${2:-$SHIELDED_FROM_EPOCH}"
  local total="0"
  local line
  while read -r value _nullifier; do
    if [[ -z "$value" ]]; then
      continue
    fi
    total="$(py -c 'import sys; print(int(sys.argv[1]) + int(sys.argv[2]))' "$total" "$value")"
  done < <(shielded_notes_for_secret "$secret" "$from_epoch")
  printf '%s' "$total"
}

wei_add() {
  py -c 'import sys; print(int(sys.argv[1]) + int(sys.argv[2]))' "$1" "$2"
}

wei_sub() {
  py -c 'import sys; print(int(sys.argv[1]) - int(sys.argv[2]))' "$1" "$2"
}

wei_lt() {
  py -c 'import sys; print(int(sys.argv[1]) < int(sys.argv[2]))' "$1" "$2"
}

shielded_commitments_file() {
  local from_epoch="${1:-$SHIELDED_FROM_EPOCH}"
  local tmp
  tmp="$(mktemp)"
  if ! shielded_logs_pairs "$from_epoch" > "$tmp"; then
    rm -f "$tmp"
    return 1
  fi
  printf '%s' "$tmp"
}

build_shielded_inputs_json() {
  local notes_file="$1"
  local commitments_file="$2"
  local secret="$3"
  local recipient="$4"
  local out_file="$5"
  py - "$notes_file" "$commitments_file" "$secret" "$recipient" "$SHIELDED_NOTE_BIN" "$out_file" <<'PY'
import json
import subprocess
import sys

notes_file, commitments_file, secret, recipient, note_bin, out_file = sys.argv[1:7]

inputs = []
with open(notes_file, "r", encoding="utf-8") as handle:
    for line in handle:
        parts = line.strip().split()
        if len(parts) < 6:
            continue
        idx, commitment, value, rho, rseed, _nullifier = parts[:6]
        path = json.loads(
            subprocess.check_output(
                [note_bin, "path", "--commitments", commitments_file, "--index", idx],
                text=True,
            )
        )
        inputs.append(
            {
                "recipient": recipient,
                "value": value,
                "rho": rho,
                "rseed": rseed,
                "secret": secret,
                "commitment": commitment,
                "path": {"elements": path["elements"], "bits": path["bits"]},
            }
        )

payload = {"inputs": inputs}
with open(out_file, "w", encoding="utf-8") as handle:
    json.dump(payload, handle)
PY
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

dec_to_hex() {
  local value="$1"
  py -c 'import sys
v=int(sys.argv[1])
print(hex(v))' "$value"
}

resolve_epoch_to_dec() {
  local value="$1"
  if [[ "$value" == "latest_state" || "$value" == "latest" ]]; then
    local status
    status="$(rpc_call_with_retry mazze_getStatus "[]")" || return 1
    local latest_hex
    latest_hex="$(printf '%s' "$status" | json_get_quiet latestState)"
    [[ -z "$latest_hex" ]] && return 1
    hex_to_dec "$latest_hex"
    return
  fi
  if [[ "$value" == "latest_confirmed" ]]; then
    local status
    status="$(rpc_call_with_retry mazze_getStatus "[]")" || return 1
    local latest_hex
    latest_hex="$(printf '%s' "$status" | json_get_quiet latestConfirmed)"
    [[ -z "$latest_hex" ]] && return 1
    hex_to_dec "$latest_hex"
    return
  fi
  if [[ "$value" == "latest_checkpoint" ]]; then
    local status
    status="$(rpc_call_with_retry mazze_getStatus "[]")" || return 1
    local latest_hex
    latest_hex="$(printf '%s' "$status" | json_get_quiet latestCheckpoint)"
    [[ -z "$latest_hex" ]] && return 1
    hex_to_dec "$latest_hex"
    return
  fi
  if [[ "$value" == 0x* || "$value" == 0X* ]]; then
    hex_to_dec "$value"
    return
  fi
  if [[ "$value" =~ ^[0-9]+$ ]]; then
    printf '%s' "$value"
    return
  fi
  return 1
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
  wallet balance --name <name> [--epoch <epoch>] [--public|--private|--all] [--from-epoch <epoch>]
  wallet transfer --name <name> --dest <base32|wallet> --amount <value> [--shielded]
  wallet unshield --name <name> --dest <base32|wallet> --amount <value>
  wallet shield-deposit --name <name> --amount <value> [--to <shielded|wallet>]
  wallet show <name>
  wallet address --name <name>
  wallet shielded-address --name <name>
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
  shield-deposit <from-secret|wallet> <amount-mazze> [shielded-output]
  shield-send [--inputs <file>] <shielded-output|wallet> <amount-mazze>

$(bold "Shielded pool (read-only):")
  root
  vkhash
  nullifier <hex32>

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
  ensure_shielded_note
  local shielded_addr
  shielded_addr="$("$SHIELDED_NOTE_BIN" address --secret "$secret" --network-id "$NETWORK_ID" 2>/dev/null || true)"
  hex_addr="${hex_addr#0x}"
  hex_addr="0x${hex_addr}"
  local dir
  dir="$(wallet_dir "$name")"
  mkdir -p "$dir"
  chmod 700 "$dir" 2>/dev/null || true
  printf '%s' "$base32_addr" > "$dir/address"
  printf '%s' "$hex_addr" > "$dir/address_hex"
  if [[ -n "$shielded_addr" ]]; then
    printf '%s' "$shielded_addr" > "$dir/shielded_address"
  fi
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
  if [[ -n "$shielded_addr" ]]; then
    echo "Shielded: $shielded_addr"
  fi
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
  local address hex shielded
  address="$(wallet_get_address "$name")"
  hex="$(wallet_get_address_hex "$name")"
  shielded="$(wallet_get_shielded_address "$name" 2>/dev/null || true)"
  echo "name: $name"
  echo "address: $address"
  echo "hex: $hex"
  if [[ -n "$shielded" ]]; then
    echo "shielded: $shielded"
  fi
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

cmd_wallet_shielded_address() {
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
    echo "Usage: wallet shielded-address <name>" >&2
    return 1
  fi
  if ! wallet_exists "$name"; then
    echo "Wallet not found: $name" >&2
    return 1
  fi
  wallet_get_shielded_address "$name"
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
  ensure_shielded_note
  local shielded_addr
  shielded_addr="$("$SHIELDED_NOTE_BIN" address --secret "$secret" --network-id "$NETWORK_ID" 2>/dev/null || true)"
  hex_addr="${hex_addr#0x}"
  hex_addr="0x${hex_addr}"
  local dir
  dir="$(wallet_dir "$name")"
  mkdir -p "$dir"
  chmod 700 "$dir" 2>/dev/null || true
  printf '%s' "$base32_addr" > "$dir/address"
  printf '%s' "$hex_addr" > "$dir/address_hex"
  if [[ -n "$shielded_addr" ]]; then
    printf '%s' "$shielded_addr" > "$dir/shielded_address"
  fi
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
  if [[ -n "$shielded_addr" ]]; then
    echo "Shielded: $shielded_addr"
  fi
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
    header_line+="$(printf '  %-*s  %-*s' "$bal_w" "public" "$bal_w" "private")"
  fi
  sep_line="$(printf '%-*s  %-*s' "$name_w" "$(repeat_char '-' "$name_w")" "$addr_w" "$(repeat_char '-' "$addr_w")")"
  if [[ "$with_balances" == "true" ]]; then
    sep_line+="$(printf '  %-*s  %-*s' "$bal_w" "$(repeat_char '-' "$bal_w")" "$bal_w" "$(repeat_char '-' "$bal_w")")"
  fi
  printf '%s\n' "$(bold "$header_line")"
  printf '%s\n' "$(dim "$sep_line")"
  local name
  for name in $(wallet_list_names); do
    local addr
    addr="$(wallet_get_address "$name")"
    printf '%-*s  %-*s' "$name_w" "$name" "$addr_w" "$addr"
    if [[ "$with_balances" == "true" ]]; then
      local bal_raw priv_raw priv_label
      bal_raw="$(rpc_call_result mazze_getBalance "[\"${addr}\",\"latest_state\"]" 2>/dev/null || true)"
      if [[ -n "$bal_raw" ]]; then
        printf '  %-*s' "$bal_w" "$(format_mazze_value "$bal_raw")"
      else
        printf '  %-*s' "$bal_w" "n/a"
      fi
      if wallet_is_encrypted "$name" && [[ -z "${MAZZE_WALLET_PASS:-}" ]]; then
        priv_label="locked"
      else
        local secret
        secret="$(wallet_get_secret "$name" 2>/dev/null || true)"
        if [[ -n "$secret" ]]; then
          priv_raw="$(shielded_balance_for_secret "$secret" "$SHIELDED_FROM_EPOCH" 2>/dev/null || true)"
          if [[ -n "$priv_raw" ]]; then
            priv_label="$(format_mazze_value "$priv_raw")"
          else
            priv_label="n/a"
          fi
        else
          priv_label="n/a"
        fi
      fi
      printf '  %-*s' "$bal_w" "$priv_label"
    fi
    printf '\n'
  done
}

cmd_wallet_balance() {
  local name=""
  local epoch="latest_state"
  local mode="all"
  local from_epoch="$SHIELDED_FROM_EPOCH"
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
      --public)
        mode="public"
        shift
        ;;
      --private)
        mode="private"
        shift
        ;;
      --all)
        mode="all"
        shift
        ;;
      --from-epoch)
        from_epoch="${2:-$SHIELDED_FROM_EPOCH}"
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
    echo "Usage: wallet balance --name <name> [--epoch <epoch>] [--public|--private|--all] [--from-epoch <epoch>]" >&2
    return 1
  fi
  if ! wallet_exists "$name"; then
    echo "Wallet not found: $name" >&2
    return 1
  fi
  if [[ "$mode" == "public" || "$mode" == "all" ]]; then
    local addr pub_raw pub_fmt
    addr="$(wallet_get_address "$name")"
    pub_raw="$(rpc_call_result mazze_getBalance "[\"${addr}\",\"${epoch}\"]")" || return 1
    pub_fmt="$(format_mazze_value "$pub_raw")"
    printf '%s\n' "$(dim "public:") $(bold "${pub_fmt}") MAZZE"
  fi
  if [[ "$mode" == "private" || "$mode" == "all" ]]; then
    local secret priv_raw priv_fmt
    secret="$(wallet_get_secret "$name")" || return 1
    priv_raw="$(shielded_balance_for_secret "$secret" "$from_epoch")"
    priv_fmt="$(format_mazze_value "$priv_raw")"
    printf '%s\n' "$(dim "private:") $(bold "${priv_fmt}") MAZZE"
  fi
}

cmd_wallet_transfer() {
  local name=""
  local dest=""
  local amount=""
  local shielded="false"
  local pk_check="0"
  local from_epoch="$SHIELDED_FROM_EPOCH"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --name|-n)
        name="${2:-}"
        shift 2
        ;;
      --shielded|--private)
        shielded="true"
        shift
        ;;
      --pk-check)
        pk_check="1"
        shift
        ;;
      --from-epoch)
        from_epoch="${2:-$SHIELDED_FROM_EPOCH}"
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
    echo "Usage: wallet transfer --name <name> --dest <base32|wallet> --amount <value> [--shielded] [--from-epoch <epoch>]" >&2
    return 1
  fi
  if ! wallet_exists "$name"; then
    echo "Wallet not found: $name" >&2
    return 1
  fi
  local amount_wei
  if ! amount_wei="$(amount_to_wei "$amount")"; then
    echo "Invalid amount: $amount" >&2
    return 1
  fi
  if [[ "$shielded" == "true" ]]; then
    local secret
    secret="$(resolve_wallet_secret "$name")" || return 1
    local dest_shielded self_shielded
    dest_shielded="$(resolve_wallet_shielded_address "$dest")" || return 1
    self_shielded="$(wallet_get_shielded_address "$name")" || return 1
    local commitments_file selected_file inputs_json
    commitments_file="$(shielded_commitments_file "$from_epoch")" || return 1
    selected_file="$(mktemp)"
    local selected_total="0"
    while read -r idx commitment value rho rseed nullifier; do
      if [[ -z "$idx" || -z "$commitment" || -z "$value" || -z "$rho" || -z "$rseed" ]]; then
        continue
      fi
      if [[ "$(wei_lt "$selected_total" "$amount_wei")" == "True" ]]; then
        selected_total="$(wei_add "$selected_total" "$value")"
        printf '%s %s %s %s %s %s\n' "$idx" "$commitment" "$value" "$rho" "$rseed" "$nullifier" >> "$selected_file"
      fi
      if [[ "$(wei_lt "$selected_total" "$amount_wei")" == "False" ]]; then
        break
      fi
    done < <(shielded_notes_full "$secret" "$from_epoch")
    if [[ "$(wei_lt "$selected_total" "$amount_wei")" == "True" ]]; then
      rm -f "$commitments_file" "$selected_file"
      echo "Insufficient private balance for shielded transfer." >&2
      return 1
    fi
    local change
    change="$(wei_sub "$selected_total" "$amount_wei")"
    local outputs values
    outputs="$dest_shielded"
    values="$amount_wei"
    if [[ "$change" != "0" ]]; then
      outputs="${outputs},${self_shielded}"
      values="${values},${change}"
    fi
    local anchor
    anchor="$(rpc_call_result mazze_call "[{\"to\":\"${SHIELDED_POOL_BASE32}\",\"data\":\"${SELECTOR_ROOT}\"},\"latest_state\"]" 2>/dev/null || true)"
    inputs_json="$(mktemp)"
    build_shielded_inputs_json "$selected_file" "$commitments_file" "$secret" "$self_shielded" "$inputs_json" || {
      rm -f "$commitments_file" "$selected_file" "$inputs_json"
      return 1
    }
    SHIELDED_OUTPUTS="$outputs" \
    SHIELDED_VALUES="$values" \
    ANCHOR="$anchor" \
    SHIELDED_INPUTS="$inputs_json" \
    MAZZE_SHIELDED_PK_CHECK="$pk_check" \
      "$SCRIPT_DIR/send-shielded.sh"
    local status=$?
    rm -f "$commitments_file" "$selected_file" "$inputs_json"
    return $status
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

cmd_wallet_unshield() {
  local name=""
  local dest=""
  local amount=""
  local pk_check="0"
  local from_epoch="$SHIELDED_FROM_EPOCH"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --name|-n)
        name="${2:-}"
        shift 2
        ;;
      --pk-check)
        pk_check="1"
        shift
        ;;
      --from-epoch)
        from_epoch="${2:-$SHIELDED_FROM_EPOCH}"
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
    echo "Usage: wallet unshield --name <name> --dest <base32|wallet> --amount <value> [--from-epoch <epoch>]" >&2
    return 1
  fi
  if ! wallet_exists "$name"; then
    echo "Wallet not found: $name" >&2
    return 1
  fi
  local amount_wei
  if ! amount_wei="$(amount_to_wei "$amount")"; then
    echo "Invalid amount: $amount" >&2
    return 1
  fi
  local dest_addr dest_hex
  dest_addr="$(resolve_wallet_address "$dest")"
  if [[ -z "$dest_addr" ]]; then
    echo "Invalid destination: $dest" >&2
    return 1
  fi
  require_base32 "$dest_addr" || return 1
  dest_hex="$(resolve_wallet_address_hex "$dest_addr")"
  if [[ -z "$dest_hex" ]]; then
    echo "Failed to resolve destination hex for $dest_addr" >&2
    return 1
  fi

  local secret self_shielded
  secret="$(resolve_wallet_secret "$name")" || return 1
  self_shielded="$(wallet_get_shielded_address "$name")" || return 1

  local commitments_file selected_file inputs_json
  commitments_file="$(shielded_commitments_file "$from_epoch")" || return 1
  selected_file="$(mktemp)"
  local selected_total="0"
  while read -r idx commitment value rho rseed nullifier; do
    if [[ -z "$idx" || -z "$commitment" || -z "$value" || -z "$rho" || -z "$rseed" ]]; then
      continue
    fi
    if [[ "$(wei_lt "$selected_total" "$amount_wei")" == "True" ]]; then
      selected_total="$(wei_add "$selected_total" "$value")"
      printf '%s %s %s %s %s %s\n' "$idx" "$commitment" "$value" "$rho" "$rseed" "$nullifier" >> "$selected_file"
    fi
    if [[ "$(wei_lt "$selected_total" "$amount_wei")" == "False" ]]; then
      break
    fi
  done < <(shielded_notes_full "$secret" "$from_epoch")
  if [[ "$(wei_lt "$selected_total" "$amount_wei")" == "True" ]]; then
    rm -f "$commitments_file" "$selected_file"
    echo "Insufficient private balance for unshield." >&2
    return 1
  fi

  local change
  change="$(wei_sub "$selected_total" "$amount_wei")"
  local anchor
  anchor="$(rpc_call_result mazze_call "[{\"to\":\"${SHIELDED_POOL_BASE32}\",\"data\":\"${SELECTOR_ROOT}\"},\"latest_state\"]" 2>/dev/null || true)"
  inputs_json="$(mktemp)"
  build_shielded_inputs_json "$selected_file" "$commitments_file" "$secret" "$self_shielded" "$inputs_json" || {
    rm -f "$commitments_file" "$selected_file" "$inputs_json"
    return 1
  }

  if [[ "$change" != "0" ]]; then
    OUTPUTS="$dest_hex" \
    VALUES="$amount_wei" \
    SHIELDED_OUTPUTS="$self_shielded" \
    SHIELDED_VALUES="$change" \
    ANCHOR="$anchor" \
    SHIELDED_INPUTS="$inputs_json" \
    MAZZE_SHIELDED_PK_CHECK="$pk_check" \
      "$SCRIPT_DIR/send-shielded.sh"
  else
    OUTPUTS="$dest_hex" \
    VALUES="$amount_wei" \
    ANCHOR="$anchor" \
    SHIELDED_INPUTS="$inputs_json" \
    MAZZE_SHIELDED_PK_CHECK="$pk_check" \
      "$SCRIPT_DIR/send-shielded.sh"
  fi
  local status=$?
  rm -f "$commitments_file" "$selected_file" "$inputs_json"
  return $status
}

cmd_wallet_shield_deposit() {
  local name=""
  local amount=""
  local dest=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --name|-n)
        name="${2:-}"
        shift 2
        ;;
      --amount|-a)
        amount="${2:-}"
        shift 2
        ;;
      --to|--dest)
        dest="${2:-}"
        shift 2
        ;;
      *)
        if [[ -z "$name" ]]; then
          name="$1"
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
  if [[ -z "$name" || -z "$amount" ]]; then
    echo "Usage: wallet shield-deposit --name <name> --amount <value> [--to <shielded|wallet>]" >&2
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
  local secret
  secret="$(resolve_wallet_secret "$name")" || return 1
  if [[ -n "$dest" ]]; then
    dest="$(resolve_wallet_shielded_address "$dest")" || return 1
    "$SCRIPT_DIR/shield-deposit.sh" "$secret" "$amount" "$dest"
    return $?
  fi
  "$SCRIPT_DIR/shield-deposit.sh" "$secret" "$amount"
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

cmd_shield_deposit() {
  local args=("$@")
  if [[ ${#args[@]} -lt 2 ]]; then
    echo "Usage: shield-deposit <from-wallet|secret> <amount> [shielded-output]" >&2
    return 1
  fi
  if [[ ${#args[@]} -ge 1 ]] && wallet_exists "${args[0]}"; then
    args[0]="$(resolve_wallet_secret "${args[0]}")"
  fi
  if [[ ${#args[@]} -ge 3 ]] && wallet_exists "${args[2]}"; then
    args[2]="$(wallet_get_shielded_address "${args[2]}")"
  fi
  "$SCRIPT_DIR/shield-deposit.sh" "${args[@]}"
}

cmd_shield_send() {
  local inputs=""
  if [[ "$1" == "--inputs" ]]; then
    inputs="${2:-}"
    shift 2
  fi
  if [[ $# -lt 2 ]]; then
    echo "Usage: shield-send [--inputs <file>] <shielded-output|wallet> <amount>" >&2
    return 1
  fi
  if [[ -z "$inputs" && -z "${SHIELDED_INPUTS:-}" ]]; then
    echo "shield-send requires --inputs <file> (inputs JSON)." >&2
    return 1
  fi
  local output
  output="$(resolve_wallet_shielded_address "$1")"
  shift
  if [[ -n "$inputs" ]]; then
    SHIELDED_INPUTS="$inputs" "$SCRIPT_DIR/send-shielded.sh" "$output" "$@"
  else
    "$SCRIPT_DIR/send-shielded.sh" "$output" "$@"
  fi
}

cmd_root() {
  rpc_call_pretty "mazze_call" "[{\"to\":\"${SHIELDED_POOL_BASE32}\",\"data\":\"${SELECTOR_ROOT}\"},\"latest_state\"]"
}

cmd_vkhash() {
  rpc_call_pretty "mazze_call" "[{\"to\":\"${SHIELDED_POOL_BASE32}\",\"data\":\"${SELECTOR_VK_HASH}\"},\"latest_state\"]"
}

cmd_nullifier() {
  local nullifier="${1:-}"
  if [[ -z "$nullifier" ]]; then
    echo "Usage: nullifier <hex32>" >&2
    return 1
  fi
  local padded
  if ! padded="$(pad32_hex "$nullifier")"; then
    echo "Invalid nullifier hex; expected <= 32 bytes." >&2
    return 1
  fi
  rpc_call_pretty "mazze_call" "[{\"to\":\"${SHIELDED_POOL_BASE32}\",\"data\":\"${SELECTOR_NULLIFIER}${padded}\"},\"latest_state\"]"
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
        unshield) shift; cmd_wallet_unshield "$@" ;;
        shield-deposit) shift; cmd_wallet_shield_deposit "$@" ;;
        show|info) shift; cmd_wallet_info "$@" ;;
        address) shift; cmd_wallet_address "$@" ;;
        shielded-address) shift; cmd_wallet_shielded_address "$@" ;;
        delete|rm) shift; cmd_wallet_delete "$@" ;;
        *) echo "Usage: wallet {new|import|list|balance|transfer|unshield|shield-deposit|show|address|shielded-address|delete}" >&2; return 1 ;;
      esac
      ;;
    balance) cmd_balance "$@" ;;
    account) cmd_account "$@" ;;
    nonce) cmd_nonce "$@" ;;
    pending) cmd_pending "$@" ;;
    pending-txs) cmd_pending_txs "$@" ;;
    send) cmd_send "$@" ;;
    shield-deposit) cmd_shield_deposit "$@" ;;
    shield-send) cmd_shield_send "$@" ;;
    tx) cmd_tx "$@" ;;
    receipt) cmd_receipt "$@" ;;
    tx-status) cmd_tx_status "$@" ;;
    watch) cmd_watch "$@" ;;
    addr) cmd_addr "$@" ;;
    dashboard) cmd_dashboard "$@" ;;
    root) cmd_root ;;
    vkhash) cmd_vkhash ;;
    nullifier) cmd_nullifier "$@" ;;
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
