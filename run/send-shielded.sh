#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

RPC_URL="${RPC_URL:-http://127.0.0.1:12539}"
GAS="${GAS:-5000000}"
SEED="${SEED:-}"
ANCHOR="${ANCHOR:-0x0000000000000000000000000000000000000000000000000000000000000000}"
FEE_MAZZE="${FEE_MAZZE:-0}"
MAZZE_DECIMALS="${MAZZE_DECIMALS:-18}"
NULLIFIERS="${NULLIFIERS:-}"
COMMITMENTS="${COMMITMENTS:-}"
SHIELDED_OUTPUTS="${SHIELDED_OUTPUTS:-}"
SHIELDED_VALUES="${SHIELDED_VALUES:-}"
SHIELDED_VALUES_MAZZE="${SHIELDED_VALUES_MAZZE:-}"
CIPHERTEXTS="${CIPHERTEXTS:-}"
OUTPUTS="${OUTPUTS:-}"
VALUES="${VALUES:-}"
VALUES_MAZZE="${VALUES_MAZZE:-}"
SHIELDED_INPUTS="${SHIELDED_INPUTS:-}"

ADDRESS1_EVM_HEX="0xfFD05dc5b53db270b52b4bC2B5068d41CEf1b240"
ADDRESS1_HEX="0x1fd05dc5b53db270b52b4bc2b5068d41cef1b240"
ADDRESS1_BASE32="MAZZE:TYPE.USER:AAT7A1SF0Y85E6FZFRF6FRJGVZA676RWJAFR8MTNAV"
PRIVATEKEY1="a2e4ef9d454646dc1da510de55ce7f3bc42db42feeb0e53243fdc4f09460afbb"
ADDRESS2_EVM_HEX="0xAc5187739fa3d138856be85F416c58Ef98E9B69B"
ADDRESS2_HEX="0x1c5187739fa3d138856be85f416c58ef98e9b69b"
ADDRESS2_BASE32="MAZZE:TYPE.USER:AASFDB5XX8V7CSEFRTYF8UNPND13V4R0XP9TG439H7"
PRIVATEKEY2="d3131c8519fc1745e8a53d00a02680198f0b23bfd16ebd09d1a11f112cb1b2ff"
SHIELDED_POOL_BASE32="MAZZE:TYPE.BUILTIN:AAEJUAAAAAAAAAAAAAAAAAAAAAAAAAAABAJ1SJV3W2"
SELECTOR_ROOT="0xebf0c717"
SELECTOR_VK_HASH="0x69b5d6d1"
SHIELDED_PK_HEX="${MAZZE_SHIELDED_PK_HEX:-$SCRIPT_DIR/shielded_pk.hex}"
SHIELDED_VK_HEX="${MAZZE_SHIELDED_VK_HEX:-$SCRIPT_DIR/shielded_vk.hex}"
export SHIELDED_PK_HEX SHIELDED_VK_HEX

OUTPUT_INPUT=""
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

if [[ -z "$SEED" ]]; then
  SEED="$(py -c 'import secrets; print(secrets.randbits(64))')"
fi

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

values_to_wei_list() {
  local list="$1"
  py -c 'import sys,decimal
vals=[v.strip() for v in sys.argv[1].split(",") if v.strip()]
decimals_raw=sys.argv[2].strip()
try:
    decimals=int(decimals_raw)
    if decimals <= 0:
        decimals=18
except Exception:
    decimals=18
out=[]
for amount in vals:
    if amount.lower().startswith("0x"):
        try:
            out.append(str(int(amount,16)))
            continue
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
    out.append(str(int(wei)))
print(",".join(out))' "$list" "$MAZZE_DECIMALS"
}

is_shielded_address() {
  local value="$1"
  if [[ "$value" == *:* || "$value" == mazze:* || "$value" == MAZZE:* ]]; then
    return 0
  fi
  if [[ "$value" == 0x* || "$value" == 0X* ]]; then
    value="$(strip_0x "$value")"
  fi
  [[ ${#value} -eq 128 ]] && is_hex "$value"
}

usage() {
  echo "Usage: $0 [shielded-output] <amount-mazze>" >&2
  echo "Requires: SHIELDED_INPUTS=<inputs.json> (see mazze-cli wallet transfer/unshield)." >&2
  echo "Short forms:" >&2
  echo "  $0 <amount-mazze>" >&2
  echo "Defaults: output=shielded address from PRIVATEKEY2" >&2
  echo "EVM refs: $ADDRESS1_EVM_HEX $ADDRESS2_EVM_HEX" >&2
}

case "$#" in
  0)
    if [[ -z "$SHIELDED_OUTPUTS" && -z "$SHIELDED_VALUES" && -z "$SHIELDED_VALUES_MAZZE" && -z "$OUTPUTS" && -z "$VALUES" && -z "$VALUES_MAZZE" ]]; then
      usage
      exit 1
    fi
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
    if is_shielded_address "$1"; then
      OUTPUT_INPUT="$1"
      AMOUNT_MAZZE="$2"
    else
      usage
      exit 1
    fi
    ;;
  *)
    usage
    exit 1
    ;;
esac

if [[ -n "$SHIELDED_OUTPUTS" ]]; then
  OUTPUT_INPUT="$SHIELDED_OUTPUTS"
fi

SHIELDED_BIN="$REPO_ROOT/target/debug/shielded_bundle"
SHIELDED_NOTE_BIN="$REPO_ROOT/target/debug/shielded_note"
VKHASH_BIN="$REPO_ROOT/target/debug/shielded_vkhash"

if [[ ! -x "$SHIELDED_BIN" ]]; then
  cargo build -p mazze-executor --bin shielded_bundle
fi

if [[ ! -x "$SHIELDED_NOTE_BIN" ]]; then
  cargo build -p mazze-executor --bin shielded_note
fi

if [[ ! -x "$VKHASH_BIN" ]]; then
  cargo build -p mazze-executor --bin shielded_vkhash
fi

vkhash_local() {
  if [[ ! -f "$SHIELDED_VK_HEX" ]]; then
    echo "Missing verifying key at $SHIELDED_VK_HEX" >&2
    return 1
  fi
  "$VKHASH_BIN" --vk "$SHIELDED_VK_HEX"
}

vkhash_chain() {
  local result
  result="$(rpc_call_result_retry mazze_call "[{\"to\":\"${SHIELDED_POOL_BASE32}\",\"data\":\"${SELECTOR_VK_HASH}\"},\"latest_state\"]" 3 0.2)" || return 1
  printf '%s' "$result"
}

run_shielded_bundle() {
  local output
  local err
  err="$(mktemp)"
  echo "Generating shielded proof (this can take a while)..." >&2
  if output="$("${cmd[@]}" 2>"$err")"; then
    rm -f "$err"
    printf '%s' "$output"
    return 0
  fi
  if grep -q "unknown arg: --shielded-outputs" "$err"; then
    cargo build -p mazze-executor --bin shielded_bundle
    rm -f "$err"
    output="$("${cmd[@]}")"
    printf '%s' "$output"
    return 0
  fi
  cat "$err" >&2
  rm -f "$err"
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

local_vk_hash="$(vkhash_local)" || exit 1
if [[ "${MAZZE_SHIELDED_PK_CHECK:-}" == "1" && -f "$SHIELDED_PK_HEX" ]]; then
  pk_vk_hash=""
  pk_cache="${SHIELDED_PK_HEX}.vkhash"
  if [[ -f "$pk_cache" && "$pk_cache" -nt "$SHIELDED_PK_HEX" ]]; then
    pk_vk_hash="$(cat "$pk_cache")"
  else
    pk_vk_hash="$("$VKHASH_BIN" --pk "$SHIELDED_PK_HEX")" || {
      echo "Failed to hash proving key at $SHIELDED_PK_HEX" >&2
      exit 1
    }
    printf '%s' "$pk_vk_hash" > "$pk_cache"
  fi
  if [[ -z "$pk_vk_hash" ]]; then
    echo "Empty proving-key hash cache ($pk_cache)." >&2
    exit 1
  fi
  if [[ "${pk_vk_hash,,}" != "${local_vk_hash,,}" ]]; then
    echo "Proving key does not match verifying key:" >&2
    echo "  pk->vk: $pk_vk_hash ($SHIELDED_PK_HEX)" >&2
    echo "  vk:     $local_vk_hash ($SHIELDED_VK_HEX)" >&2
    echo "Regenerate both keys with shielded_keygen and re-genesis." >&2
    exit 1
  fi
fi
chain_vk_hash="$(vkhash_chain)" || exit 1
if [[ -z "$chain_vk_hash" || "$chain_vk_hash" == "null" ]]; then
  echo "Failed to read on-chain vkhash from shielded pool." >&2
  exit 1
fi
if [[ "$chain_vk_hash" == "0x0" || "$chain_vk_hash" == "0x0000000000000000000000000000000000000000000000000000000000000000" ]]; then
  echo "On-chain vkhash is zero; genesis did not set the verifying key." >&2
  echo "Re-genesis with matching run/shielded_vk.hex and run/shielded_pk.hex." >&2
  exit 1
fi
if [[ "${chain_vk_hash,,}" != "${local_vk_hash,,}" ]]; then
  echo "Verifying key mismatch:" >&2
  echo "  chain: $chain_vk_hash" >&2
  echo "  local: $local_vk_hash ($SHIELDED_VK_HEX)" >&2
  echo "Re-genesis with matching shielded_vk.hex/shielded_pk.hex before sending shielded txs." >&2
  exit 1
fi

if [[ -z "$OUTPUT_INPUT" && -z "$SHIELDED_OUTPUTS" ]]; then
  if [[ -z "$OUTPUTS" && -z "$VALUES" && -z "$VALUES_MAZZE" ]]; then
    OUTPUT_INPUT="$("$SHIELDED_NOTE_BIN" address --secret "$PRIVATEKEY2" --network-id "$network_id")"
  fi
fi
if [[ -n "$OUTPUT_INPUT" && -z "$SHIELDED_OUTPUTS" ]]; then
  SHIELDED_OUTPUTS="$OUTPUT_INPUT"
fi
if [[ -z "$SHIELDED_VALUES" && -z "$SHIELDED_VALUES_MAZZE" && -z "$OUTPUTS" && -z "$VALUES" && -z "$VALUES_MAZZE" ]]; then
  SHIELDED_VALUES_MAZZE="$AMOUNT_MAZZE"
fi
if [[ -n "$SHIELDED_VALUES_MAZZE" && -z "$SHIELDED_VALUES" ]]; then
  SHIELDED_VALUES="$(values_to_wei_list "$SHIELDED_VALUES_MAZZE")" || {
    echo "Invalid shielded amount(s): $SHIELDED_VALUES_MAZZE" >&2
    exit 1
  }
  SHIELDED_VALUES_MAZZE=""
fi
if [[ -n "$VALUES_MAZZE" && -z "$VALUES" ]]; then
  VALUES="$(values_to_wei_list "$VALUES_MAZZE")" || {
    echo "Invalid transparent amount(s): $VALUES_MAZZE" >&2
    exit 1
  }
  VALUES_MAZZE=""
fi

FEE_RAW="0"
if [[ -n "$FEE_MAZZE" ]]; then
  FEE_RAW="$(amount_to_wei "$FEE_MAZZE")" || {
    echo "Invalid fee: $FEE_MAZZE" >&2
    exit 1
  }
fi

if [[ "$ANCHOR" == "0x0000000000000000000000000000000000000000000000000000000000000000" ]]; then
  root="$(rpc_call_result_retry mazze_call "[{\"to\":\"${SHIELDED_POOL_BASE32}\",\"data\":\"${SELECTOR_ROOT}\"},\"latest_state\"]" 3 0.2 2>/dev/null || true)"
  if [[ -n "$root" && "$root" != "null" ]]; then
    ANCHOR="$root"
  fi
fi

has_shielded="false"
has_public="false"
if [[ -n "$SHIELDED_OUTPUTS" || -n "$SHIELDED_VALUES" ]]; then
  has_shielded="true"
fi
if [[ -n "$OUTPUTS" || -n "$VALUES" ]]; then
  has_public="true"
fi
if [[ "$has_shielded" == "false" && "$has_public" == "false" ]]; then
  echo "No outputs specified (shielded or transparent)." >&2
  exit 1
fi
if [[ "$has_shielded" == "true" ]]; then
  if [[ -z "$SHIELDED_OUTPUTS" || -z "$SHIELDED_VALUES" ]]; then
    echo "Shielded outputs and values are required." >&2
    exit 1
  fi
fi
if [[ "$has_public" == "true" ]]; then
  if [[ -z "$OUTPUTS" || -z "$VALUES" ]]; then
    echo "Transparent outputs and values are required." >&2
    exit 1
  fi
fi
if [[ -z "$SHIELDED_INPUTS" ]]; then
  echo "Missing shielded inputs; set SHIELDED_INPUTS to the inputs JSON." >&2
  exit 1
fi

cmd=("$SHIELDED_BIN"
  --fee "$FEE_RAW"
  --anchor "$ANCHOR"
  --gas "$GAS"
  --seed "$SEED"
  --chain-id "$chain_id"
  --epoch "$epoch"
)
cmd+=(--inputs "$SHIELDED_INPUTS")

if [[ "$has_public" == "true" ]]; then
  cmd+=(--outputs "$OUTPUTS" --values "$VALUES")
fi
if [[ "$has_shielded" == "true" ]]; then
  cmd+=(--shielded-outputs "$SHIELDED_OUTPUTS" --shielded-values "$SHIELDED_VALUES")
fi

if [[ -n "$COMMITMENTS" ]]; then
  cmd+=(--commitments "$COMMITMENTS")
fi
if [[ -n "$CIPHERTEXTS" ]]; then
  cmd+=(--ciphertexts "$CIPHERTEXTS")
fi

raw_tx="$(run_shielded_bundle)"
send_result="$(rpc_call_result_retry mazze_sendRawTransaction "[\"${raw_tx}\"]" 12 0.5)"
echo "$send_result"
