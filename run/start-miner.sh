#!/usr/bin/env bash
set -euo pipefail

ulimit -n 200000 2>/dev/null || ulimit -n 10240 2>/dev/null || true

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
EXECUTABLE="$REPO_ROOT/target/release/mazze-miner"
CONFIG_FILE="$SCRIPT_DIR/hydra.toml"
LOG_DIR="$REPO_ROOT/run/logs"
PID_FILE="$SCRIPT_DIR/miner_pid.txt"
LOG_FILE="$LOG_DIR/mazze-miner.log"

mkdir -p "$LOG_DIR"

if [[ ! -x "$EXECUTABLE" ]]; then
  echo "Error: binary not found at $EXECUTABLE. Did you run: cargo build --release?" >&2
  exit 1
fi

if [[ ! -f "$CONFIG_FILE" ]]; then
  echo "Error: config not found at $CONFIG_FILE" >&2
  exit 1
fi

echo "-------$(date '+%Y-%m-%d %H:%M:%S')-------" >> "$LOG_FILE"

SECRETS_FILE="$SCRIPT_DIR/hydra.secrets.toml"
ensure_stratum_secret() {
  if [[ ! -f "$SECRETS_FILE" ]]; then
    echo "Generating $SECRETS_FILE (first-run stratum secret)..." >&2
    local generated
    if command -v openssl >/dev/null 2>&1; then
      generated="$(openssl rand -hex 32)"
    else
      generated="$(head -c 32 /dev/urandom | xxd -p -c 64)"
    fi
    umask 077
    cat > "$SECRETS_FILE" <<EOF
# Auto-generated per-deployment secrets. Do not commit.
# See run/hydra.secrets.toml.example for the format and rotation guidance.
stratum_secret = "$generated"
EOF
    chmod 600 "$SECRETS_FILE"
  fi
  STRATUM_SECRET="$(grep -E '^[[:space:]]*stratum_secret[[:space:]]*=' "$SECRETS_FILE" \
    | head -n1 \
    | sed -E 's/^[[:space:]]*stratum_secret[[:space:]]*=[[:space:]]*"([^"]*)".*/\1/')"
  if [[ -z "$STRATUM_SECRET" ]]; then
    echo "Error: stratum_secret missing or unparseable in $SECRETS_FILE" >&2
    exit 1
  fi
}
ensure_stratum_secret

# Merge the template with the secret into a per-run config the miner reads.
TEMP_CONF="$SCRIPT_DIR/hydra.miner.runtime.toml"
cp "$CONFIG_FILE" "$TEMP_CONF"
if grep -q '^[[:space:]]*stratum_secret[[:space:]]*=' "$TEMP_CONF"; then
  sed -i.bak "s#^[[:space:]]*stratum_secret[[:space:]]*=.*#stratum_secret = \"$STRATUM_SECRET\"#" "$TEMP_CONF"
else
  printf '\nstratum_secret = "%s"\n' "$STRATUM_SECRET" >> "$TEMP_CONF"
fi

# RANDOMX_FULL_MEM can be exported by the user to control miner memory usage.
rm -f "$TEMP_CONF.bak"
pushd "$REPO_ROOT" >/dev/null

# The miner must never starve the node it mines for. At the same priority and
# with ~1 thread per core, the node got 13-37% of one core: enough to follow
# the tip, but peers catching up from it saw requests time out for minutes
# (2026-10-10, m1 restart could not catch up). Lowest priority + 2 free cores.
CPUS=$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 2)
DEFAULT_THREADS=$(( CPUS - 2 )); [ "$DEFAULT_THREADS" -lt 1 ] && DEFAULT_THREADS=1
RANDOMX_FULL_MEM="${RANDOMX_FULL_MEM:-0}" RUST_LOG=info \
nice -n "${MINER_NICE:-19}" \
"$EXECUTABLE" --config "$TEMP_CONF" --worker-id "${WORKER_ID:-1}" --num-threads "${NUM_THREADS:-$DEFAULT_THREADS}" \
  >> "$LOG_FILE" 2>&1 &

PID=$!
echo "$PID" > "$PID_FILE"
echo "Mazze miner started (pid=$PID). Logs: $LOG_FILE"

popd >/dev/null
