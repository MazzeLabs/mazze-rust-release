#!/usr/bin/env bash
# Local multi-node harness: "restart a node with its existing DB after the
# chain has moved on by N eras" (archive restart / CatchUp* phase path).
#
# Topology (all on 127.0.0.1, mode = "test"):
#   n1, n2 : full nodes, CPU-mining at difficulty 1
#   n4     : full node, no mining (keeps every miner at >= 2 peers while the
#            archives are down; the isolation guard drops mined blocks below 2)
#   ARCHIVES (default "3")      : archive nodes stopped at STOP_AT_EPOCH and
#                                 restarted after the chain moved ADVANCE epochs
#   EARLY_ARCHIVES (default "") : archive nodes stopped at the same time but
#                                 restarted after EARLY_ADVANCE epochs, so their
#                                 sync target is still covered by their OWN
#                                 local snapshot/commitment
#
# Consensus parameters are production ones scaled by ERA/20000: era =
# snapshot = ERA epochs (default 2000 via the test-only
# `test_allow_small_snapshot_epoch_count`; ERA=20000 = production sizes) and
# timer_chain_beta = 100*ERA/20000, so the stable checkpoint trails the tip
# by ~1-3 eras like on the fleet. Test mode pins the difficulty (no ASERT).
# RandomX runs light-only (MAZZE_RANDOMX_LIGHT_ONLY=1, test-only switch) so
# the nodes fit in RAM.
#
# Usage:
#   scripts/local-restart-repro.sh run              # full scenario
#   scripts/local-restart-repro.sh replay           # redo only the restart
#   scripts/local-restart-repro.sh status | timeline <n> | clean
#   scripts/local-restart-repro.sh start-node <n> | stop-node <n>
#   JOINERS=5 scripts/local-restart-repro.sh fastsync   # fast-sync joiner(s)
#   JOINERS=6 scripts/local-restart-repro.sh pick-anchors && \
#     JOINERS=6 scripts/local-restart-repro.sh start-node 6  # one more joiner
#
# Knobs (env):
#   PORT_BASE=20000          node n listens on PORT_BASE + 100*n + offset
#   JOINERS="5"              fresh nodes started with only trusted_checkpoints
#                            ($WORK/anchors, see pick-anchors); JOINER_TYPE
#                            (default full), JOINER_MINE (default 1)
#   FS_EPOCHS=6*ERA          fastsync: chain length before the joiners start
#   MDBX_SYNC_MODE=          e.g. safenosync / utterlynosync for a shared disk
#   WORK=<dir>               data/log root (default run/local-repro)
#   BIN=<path>               mazze binary (default target/release/mazze);
#   BIN_<n>=<path>           per-node override, e.g. BIN_3=/tmp/old/mazze
#   ERA=2000                 era/snapshot size in epochs
#   STOP_AT_EPOCH=2.5*ERA    stop the archives at this epoch
#   ADVANCE=2*ERA            restart ARCHIVES after the chain moved this far
#   EARLY_ADVANCE=ERA/8      restart EARLY_ARCHIVES after this many epochs
#   FREEZE=1                 before restarting ARCHIVES, wait until n4 serves
#                            the snapshot at the next era boundary, then stop
#                            the miners until the archives reach Normal (the
#                            restarted node's header ingest is slower than
#                            local block production, so without this it never
#                            catches up); miners restart before the STAY_S check
#   CLONE="3:5"              before the restart, replace n3's DB with a copy of
#                            n5's (fresh node key), so two binaries restart from
#                            the IDENTICAL database (A/B of baseline vs fix)
#   NORMAL_TIMEOUT_S=1800    wait this long for the archives to reach Normal
#   STAY_S=300               then check they keep up for this long
#   LOG_EXTRA_<n>            extra log.yaml loggers, "target: level;..."
#   ASERT=true               run per-block ASERT (test_enable_asert) instead of
#                            the pinned test-mode difficulty
#
# Example (baseline vs fix in one run, plus the covered-target variant):
#   BIN_3=/path/old/mazze ARCHIVES="3 5" EARLY_ARCHIVES=6 \
#     scripts/local-restart-repro.sh run
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="${BIN:-$REPO_ROOT/target/release/mazze}"
BASE_CONF="$REPO_ROOT/run/hydra.toml"
WORK="${WORK:-$REPO_ROOT/run/local-repro}"
ERA="${ERA:-2000}"
STOP_AT_EPOCH="${STOP_AT_EPOCH:-$(( ERA * 5 / 2 ))}"
ADVANCE="${ADVANCE:-$(( ERA * 2 ))}"
EARLY_ADVANCE="${EARLY_ADVANCE:-$(( ERA / 8 ))}"
ARCHIVES="${ARCHIVES:-3}"
EARLY_ARCHIVES="${EARLY_ARCHIVES:-}"
NORMAL_TIMEOUT_S="${NORMAL_TIMEOUT_S:-1800}"
FREEZE="${FREEZE:-0}"
CLONE="${CLONE:-}"
STAY_S="${STAY_S:-300}"
AUTHOR="mazze:aamzbekyhkk2ag8rzwvyetyecrvncyg0hjpw84k5yr"
PERMANENT="1 2 4"
JOINERS="${JOINERS:-}"

export MAZZE_RANDOMX_LIGHT_ONLY=1
export MAZZE_SHIELDED_VK_HEX="${MAZZE_SHIELDED_VK_HEX:-$REPO_ROOT/run/shielded_vk.hex}"
ulimit -n 200000 2>/dev/null || true

PORT_BASE="${PORT_BASE:-20000}"
port() { echo $((PORT_BASE + $1 * 100 + $2)); }   # port <node> <offset>; below the ephemeral range (32768+) so outgoing connections cannot steal a listen port
rpc_port() { port "$1" 39; }
ndir() { echo "$WORK/n$1"; }
log() { echo "[$(date '+%H:%M:%S')] $*"; }
is_archive() { [[ " $ARCHIVES $EARLY_ARCHIVES " == *" $1 "* ]]; }
is_joiner() { [[ " $JOINERS " == *" $1 "* ]]; }
bin_of() { local v="BIN_$1"; echo "${!v:-$BIN}"; }
all_nodes() { echo "$PERMANENT $ARCHIVES $EARLY_ARCHIVES $JOINERS"; }

rpc() { # rpc <node> <method> [params-json]
  curl -s -m 5 -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":${3:-[]}}" \
    "http://127.0.0.1:$(rpc_port "$1")" 2>/dev/null || true
}
epoch() { # latest_mined epoch of node, or -1
  local r; r="$(rpc "$1" mazze_epochNumber '["latest_mined"]')"
  r="$(sed -nE 's/.*"result":"0x([0-9a-fA-F]+)".*/\1/p' <<<"$r")"
  [[ -n "$r" ]] && echo $((16#$r)) || echo -1
}
phase() { # sync phase of the CURRENT process (since its last start marker)
  { awk '/^------- start/{p=""} /start phase "/{p=$0} END{print p}' "$(ndir "$1")/mazze.log" 2>/dev/null || true; } \
    | { grep -oE 'start phase "[A-Za-z]+' || true; } | awk '{print $3}' | tr -d '"'; }
# The launcher may wrap the process, so find it by its unique config path.
pid_of() { pgrep -f -- "--config $(ndir "$1")/hydra.toml" | head -1 || true; }
alive() { [[ -n "$(pid_of "$1")" ]]; }

write_log_yaml() { # write_log_yaml <node>
  local d; d="$(ndir "$1")"
  local v="LOG_EXTRA_$1"
  {
    cat <<EOF
refresh_rate: 30 seconds
appenders:
  logfile:
    kind: rolling_file
    path: "$d/mazze.log"
    encoder:
      pattern: "{d} {h({l}):5.5} {T:<20.20} {M} - {m:.20000}{n}"
    policy:
      kind: compound
      trigger: { kind: size, limit: 2000 mb }
      roller: { kind: fixed_window, pattern: "$d/mazze.{}.log.gz", count: 3 }
root:
  level: info
  appenders: [logfile]
loggers:
  network: { level: info }
  mazzecore: { level: info }
EOF
    tr ';' '\n' <<<"${!v:-}" | sed -nE 's/^ *(.+[^:]): *([a-z]+) *$/  "\1": { level: \2 }/p'
  } > "$d/log.yaml"
}

write_conf() { # write_conf <node> <bootnodes>
  local i="$1" boot="$2" d; d="$(ndir "$i")"
  mkdir -p "$d"
  local c="$d/hydra.toml"
  cp "$BASE_CONF" "$c"
  set_kv() {
    if grep -q "^[[:space:]]*$1[[:space:]]*=" "$c"; then
      sed -i "s#^[[:space:]]*$1[[:space:]]*=.*#$1 = $2#" "$c"
    else printf '\n%s = %s\n' "$1" "$2" >> "$c"; fi
  }
  set_kv mode '"test"'
  set_kv initial_difficulty 1
  set_kv era_epoch_count "$ERA"
  set_kv snapshot_epoch_count "$ERA"
  set_kv test_allow_small_snapshot_epoch_count true
  # ASERT=true: per-block ASERT in test mode (test_enable_asert), to check
  # that restarted nodes agree with the network on every block's difficulty.
  set_kv test_enable_asert "${ASERT:-false}"
  set_kv timer_chain_beta "$(( 100 * ERA / 20000 > 2 ? 100 * ERA / 20000 : 2 ))"
  set_kv bootnodes "\"$boot\""
  set_kv public_address '"127.0.0.1"'
  set_kv session_ip_limits '"0,0,0,0"'
  set_kv subnet_quota 0   # node db otherwise keeps only 1 node per IP
  set_kv tcp_port "$(port "$i" 0)"
  set_kv udp_port "$(port "$i" 0)"
  set_kv stratum_port "$(port "$i" 25)"
  set_kv jsonrpc_ws_port "$(port "$i" 35)"
  set_kv jsonrpc_tcp_port "$(port "$i" 36)"
  set_kv jsonrpc_http_port "$(port "$i" 37)"
  set_kv jsonrpc_local_http_port "$(rpc_port "$i")"
  set_kv jsonrpc_http_eth_port "$(port "$i" 45)"
  set_kv jsonrpc_ws_eth_port "$(port "$i" 46)"
  set_kv mazze_data_dir "\"$d/data\""
  set_kv log_conf "\"$d/log.yaml\""
  set_kv max_outgoing_peers 6
  set_kv mdbx_map_size_mb 16384
  # MDBX_SYNC_MODE=safenosync: no fsync per commit. On a disk shared with
  # other clusters, durable commits made a tiny snapshot merge take ~9 min,
  # longer than a 500-epoch snapshot period: the executor then skipped
  # epochs ('Consensus state init failed, skipping epoch') and halted.
  if [[ -n "${MDBX_SYNC_MODE:-}" ]]; then
    set_kv mdbx_sync_mode "\"$MDBX_SYNC_MODE\""
  fi
  # On restart the genesis is looked up by block number. With
  # persist_block_number_index=false (run/hydra.toml) it is not found, and
  # with execute_genesis=false (required once chain data exists) the node
  # recomputes it WITHOUT the genesis txs -> different genesis hash -> every
  # peer rejects it ("genesis hash mismatches").
  set_kv persist_block_number_index true
  if compgen -G "$d/data/storage_db/*" >/dev/null; then
    set_kv execute_genesis false
  else
    set_kv execute_genesis true
  fi
  if is_joiner "$i"; then
    # Fresh fast-sync joiner: nothing but the trusted checkpoints.
    set_kv node_type "\"${JOINER_TYPE:-full}\""
    set_kv trusted_checkpoints "\"$(cat "$WORK/anchors")\""
    if [[ "${JOINER_MINE:-1}" == 1 ]]; then
      set_kv mining_type '"cpu"'
      set_kv mining_author "\"$AUTHOR\""
    else
      set_kv mining_type '"disable"'
    fi
  elif is_archive "$i"; then
    set_kv node_type '"archive"'
    set_kv mining_type '"disable"'
  elif [[ "$i" == 4 ]]; then
    set_kv node_type '"full"'
    set_kv mining_type '"disable"'
  else
    set_kv node_type '"full"'
    set_kv mining_type '"cpu"'
    set_kv mining_author "\"$AUTHOR\""
  fi
  write_log_yaml "$i"
}

start_node() { # start_node <node>
  local i="$1" d; d="$(ndir "$i")"
  if alive "$i"; then log "n$i already running"; return; fi
  local boot="" j id
  for j in $PERMANENT; do
    [[ "$j" == "$i" ]] && continue
    id="$(grep -ohE 'Self node id: 0x[0-9a-f]+' "$(ndir "$j")/mazze.log" 2>/dev/null | tail -1 | awk '{print $4}' || true)"
    [[ -n "$id" ]] && boot="${boot:+$boot,}mazzenode://${id#0x}@127.0.0.1:$(port "$j" 0)"
  done
  [[ "$i" != 1 && -z "$boot" ]] && { echo "no bootnode ids yet; start n1 first" >&2; exit 1; }
  write_conf "$i" "$boot"
  echo "------- start $(date '+%F %T') bin=$(bin_of "$i") -------" | tee -a "$d/stdout.log" >> "$d/mazze.log"
  (cd "$d" && setsid nohup "$(bin_of "$i")" --config "$d/hydra.toml" >> "$d/stdout.log" 2>&1 &)
  local n_starts; n_starts="$(grep -c -- '------- start' "$d/mazze.log" || true)"
  for _ in $(seq 1 120); do
    sleep 1
    (( $(grep -c 'Self node id' "$d/mazze.log" || true) >= n_starts )) && break
    alive "$i" || { log "n$i failed to start:"; tail -5 "$d/stdout.log"; return 1; }
  done
  alive "$i" || { log "n$i failed to start:"; tail -3 "$d/stdout.log"; return 1; }
  log "started n$i pid=$(pid_of "$i") rpc=$(rpc_port "$i") bin=$(bin_of "$i")"
}

stop_node() { # graceful (SIGTERM, then SIGKILL after 120s)
  local i="$1" p; p="$(pid_of "$i")"
  [[ -z "$p" ]] && return 0
  kill "$p" 2>/dev/null || true
  for _ in $(seq 1 120); do alive "$i" || break; sleep 1; done
  if alive "$i"; then log "n$i SIGKILL"; kill -9 "$(pid_of "$i")" || true; fi
  log "stopped n$i"
}

status() {
  local i
  for i in $(all_nodes); do
    printf 'n%s alive=%s epoch=%s phase=%s\n' "$i" \
      "$(alive "$i" && echo y || echo n)" "$(epoch "$i")" "$(phase "$i")"
  done
}

restart_log() { # log of node <n> since its last RESTART marker
  awk '/------- RESTART/{buf=""} {buf=buf $0 "\n"} END{printf "%s", buf}' "$(ndir "$1")/mazze.log"
}

timeline() { # key restart-path events of node <n> since its last RESTART
  restart_log "$1" \
    | grep -E 'start phase|re-anchor|Fail to recover state_valid|skip state sync|WEDGED|panicked|request_epochs exit|for the consensus worker to drain|already on the local consensus|Checkpoint sync' \
    | sed -E 's/^[0-9-]+T([0-9:]{8})[^ ]* +([A-Z]+) .* - /\1 \2 /' | cut -c1-240 | uniq | tail -n "${2:-40}"
}

wait_epoch() { # wait_epoch <node> <target>
  local t0; t0=$(date +%s)
  while :; do
    local e; e="$(epoch "$1")"
    (( e >= $2 )) && { log "n$1 reached epoch $e"; return; }
    if (( $(date +%s) - t0 >= 60 )); then
      t0=$(date +%s); log "waiting n$1 -> $2 | $(status | tr '\n' ' ')"
    fi
    local i; for i in $PERMANENT; do
      alive "$i" || { echo "n$i died" >&2; tail -20 "$(ndir "$i")/stdout.log" >&2; exit 1; }
    done
    sleep 5
  done
}

restart_archive() {
  echo "------- RESTART $(date '+%F %T') -------" >> "$(ndir "$1")/mazze.log"
  start_node "$1"
}

check_archives() { # check_archives <ids...>: wait for Normal, then in-sync
  local rc=0 i t0; t0=$(date +%s)
  declare -A normal_at=()
  while (( $(date +%s) - t0 < NORMAL_TIMEOUT_S )); do
    local pending=0
    for i in "$@"; do
      [[ -n "${normal_at[$i]:-}" ]] && continue
      if ! alive "$i"; then log "n$i DIED"; tail -5 "$(ndir "$i")/stdout.log"; normal_at[$i]=dead; continue; fi
      if [[ "$(phase "$i")" == NormalSyncPhase ]]; then
        normal_at[$i]=$(( $(date +%s) - t0 )); log "n$i reached Normal after ${normal_at[$i]}s"
      else pending=1; fi
    done
    (( pending )) || break
    sleep 15
    log "$(status | tr '\n' ' ')"
  done
  if [[ "$FREEZE" == 1 ]]; then
    start_node 1 || { sleep 30; start_node 1; }   # port may still be held
    start_node 2 || { sleep 30; start_node 2; }
  fi
  if (( STAY_S > 0 )); then log "checking the archives keep up for ${STAY_S}s"; sleep "$STAY_S"; fi
  local tip; tip="$(epoch 4)"
  for i in "$@"; do
    echo "=== n$i ($(bin_of "$i")) timeline since restart ==="; timeline "$i" 30
    local e; e="$(epoch "$i")"
    if [[ -z "${normal_at[$i]:-}" || "${normal_at[$i]}" == dead ]]; then
      log "RESULT n$i: FAIL (no Normal; phase=$(phase "$i") epoch=$e tip=$tip)"; rc=2
    elif (( e + 300 < tip )); then
      log "RESULT n$i: FAIL (Normal after ${normal_at[$i]}s but lagging: epoch=$e tip=$tip)"; rc=3
    else
      log "RESULT n$i: PASS (Normal after ${normal_at[$i]}s; epoch=$e tip=$tip)"
    fi
  done
  return $rc
}

run() {
  mkdir -p "$WORK"
  local i
  for i in $PERMANENT; do alive "$i" || start_node "$i"; done
  # Archives join a running chain (several fresh nodes racing at genesis
  # sometimes wedge early; see the report).
  wait_epoch 1 100
  for i in $ARCHIVES $EARLY_ARCHIVES; do alive "$i" || start_node "$i"; done
  wait_epoch "${ARCHIVES%% *}" "$STOP_AT_EPOCH"
  local stop_e; stop_e="$(epoch 1)"
  for i in $ARCHIVES $EARLY_ARCHIVES; do log "n$i at epoch $(epoch "$i")"; stop_node "$i"; done
  local rc=0
  if [[ -n "$EARLY_ARCHIVES" ]]; then
    wait_epoch 1 $(( stop_e + EARLY_ADVANCE ))
    for i in $EARLY_ARCHIVES; do restart_archive "$i"; done
    check_archives $EARLY_ARCHIVES || rc=$?
  fi
  wait_epoch 1 $(( stop_e + ADVANCE ))
  if [[ "$FREEZE" == 1 ]]; then
    local want=$(( (stop_e / ERA + 1) * ERA )) snap
    while :; do
      snap="$(rpc 4 mazze_getStatus | grep -oE '"latestSnapshotEpochNumber":"0x[0-9a-f]+"' | grep -oE '0x[0-9a-f]+' || true)"
      (( ${snap:-0} >= want )) && break
      log "waiting for n4 to serve snapshot $want (latest ${snap:-none})"; sleep 30
    done
    log "freezing the chain: stopping miners at epoch $(epoch 1)"
    stop_node 1; stop_node 2
  fi
  local pair
  for pair in $CLONE; do
    local dst="${pair%%:*}" src="${pair##*:}"
    log "cloning n$src's DB into n$dst"
    rm -rf "$(ndir "$dst")/data"
    cp -a "$(ndir "$src")/data" "$(ndir "$dst")/data"
    rm -rf "$(ndir "$dst")/data/net_config"
  done
  for i in $ARCHIVES; do
    rm -rf "$(ndir "$i")/data.pre-restart"
    cp -a "$(ndir "$i")/data" "$(ndir "$i")/data.pre-restart"
  done
  for i in $ARCHIVES; do restart_archive "$i"; done
  check_archives $ARCHIVES || rc=$?
  return $rc
}

status_field() { # status_field <node> <camelCaseField>: first numeric hit
  local v; v="$(rpc "$1" mazze_getStatus | grep -oE "\"$2\":\"0x[0-9a-f]+\"" | head -1 | grep -oE '0x[0-9a-f]+' || true)"
  [[ -n "$v" ]] && echo $((v)) || echo -1
}
main_block() { # main_block <node> <epoch>: "<hash> <blockNumber>"
  local r; r="$(rpc "$1" mazze_getBlockByEpochNumber "[\"$(printf '0x%x' "$2")\",false]")"
  echo "$(grep -oE '"hash":"0x[0-9a-f]{64}"' <<<"$r" | head -1 | grep -oE '0x[0-9a-f]{64}')" \
    "$(( $(grep -oE '"blockNumber":"0x[0-9a-f]+"' <<<"$r" | head -1 | grep -oE '0x[0-9a-f]+') ))"
}

# Write $WORK/anchors from archive n3, the way an operator harvests them:
# the newest era checkpoint the network has made (latestCheckpoint) and the
# era before it, each "height:hash:blockNumber". ANCHOR_FORMAT=legacy writes
# the old "height:hash:blame_height:blame_hash" form (blame = height + ERA +
# BLAME_MARGIN) for binaries before the consensus-anchored fast sync.
pick_anchors() {
  local cp list="" k a="${ARCHIVES%% *}"
  cp="$(status_field "$a" latestCheckpoint)"
  for k in 0 1 2; do
    local c=$(( cp / ERA * ERA - k * ERA )) hash bn b bhash
    (( c <= 0 )) && continue
    read -r hash bn <<<"$(main_block "$a" "$c")"
    if [[ "${ANCHOR_FORMAT:-}" == legacy ]]; then
      b=$(( c + ERA + ${BLAME_MARGIN:-100} ))
      read -r bhash _ <<<"$(main_block "$a" "$b")"
      list="${list:+$list,}$c:$hash:$b:$bhash"
    else
      list="${list:+$list,}$c:$hash:$bn"
    fi
  done
  echo "$list" > "$WORK/anchors"
  log "anchors (checkpoint $cp): $list"
}

# Fast-sync scenario: a chain of FS_EPOCHS epochs served by archive n3,
# then fresh joiners ($JOINERS) with only trusted_checkpoints must reach
# Normal, follow the tip and (by default) mine.
fastsync() {
  mkdir -p "$WORK"
  local i
  for i in $PERMANENT; do alive "$i" || start_node "$i"; done
  wait_epoch 1 50
  for i in $ARCHIVES; do alive "$i" || start_node "$i"; done
  wait_epoch "${ARCHIVES%% *}" "${FS_EPOCHS:-$(( ERA * 6 ))}"
  [[ -s "$WORK/anchors" && "${REPICK:-1}" == 0 ]] || pick_anchors
  for i in $JOINERS; do
    stop_node "$i"; rm -rf "$(ndir "$i")"; mkdir -p "$(ndir "$i")"
    start_node "$i"
  done
  check_archives $JOINERS
}

# Re-run only the restart step from the DB copies `run` saved right before
# it restarted the archives (data.pre-restart), e.g. with a new binary.
# Honours FREEZE (miners must be stopped again; the chain must not have
# moved much, or the archives are no longer "N eras behind").
replay() {
  local i
  [[ "$FREEZE" == 1 ]] && { stop_node 1; stop_node 2; }
  for i in $ARCHIVES; do
    stop_node "$i"
    [[ -d "$(ndir "$i")/data.pre-restart" ]] || { echo "no pre-restart copy for n$i" >&2; exit 1; }
    rm -rf "$(ndir "$i")/data"
    cp -a "$(ndir "$i")/data.pre-restart" "$(ndir "$i")/data"
  done
  for i in $ARCHIVES; do restart_archive "$i"; done
  check_archives $ARCHIVES
}

case "${1:-run}" in
  run) run ;;
  fastsync) fastsync ;;
  pick-anchors) pick_anchors ;;
  replay) replay ;;
  start) for i in $PERMANENT $ARCHIVES $EARLY_ARCHIVES; do start_node "$i"; done ;;
  stop) for i in $JOINERS $EARLY_ARCHIVES $ARCHIVES 4 2 1; do stop_node "$i"; done ;;
  start-node) start_node "$2" ;;
  restart-node) restart_archive "$2" ;;
  stop-node) stop_node "$2" ;;
  status) status ;;
  timeline) timeline "${2:-3}" "${3:-40}" ;;
  clean) for i in $JOINERS $EARLY_ARCHIVES $ARCHIVES 4 2 1; do stop_node "$i"; done; rm -rf "$WORK" ;;
  *) sed -n 2,44p "$0"; exit 1 ;;
esac
