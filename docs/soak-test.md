# High-BPS Soak Test

This soak harness drives sustained block production and transaction load while emitting metrics for GC/DB tuning.

## Run

Build a release binary first:

```
cargo build --release
```

Start the soak node:

```
TARGET_BPS=8 TX_PERIOD_US=1000 TXGEN_ACCOUNT_COUNT=200 ./run/start-soak.sh
```

Stop it:

```
kill "$(cat run/soak_pid.txt)"
```

## Metrics output

Metrics are written to `logs/metrics-soak.log` (append-only). Useful signals:

- `sync_graph` group: `arena_size`, `old_era_frontier_size`, `not_ready_frontier_size`
- `consensus_worker_queue` group: `queued`, `enq_tps`, `deq_tps`
- `timer` group: `consensus::state_commit_time_expdec.*`
- `storage_mpt` group: `cache_hit_rate_pct`, `cache_misses`, `db_loads`

## Tuning knobs

The soak script exposes common GC/DB knobs via environment variables:

- `MDBX_MAP_SIZE_MB` (default 65536)
- `BLOCK_CACHE_GC_MS`
- `STORAGE_MAX_OPEN_SNAPSHOTS`
- `STORAGE_MAX_OPEN_MPT_COUNT`
- `STORAGE_DELTA_MPTS_CACHE_SIZE`
- `STORAGE_DELTA_MPTS_CACHE_START_SIZE`
- `STORAGE_DELTA_MPTS_SLAB_IDLE_SIZE`

## Tuning signals

- `sync_graph.old_era_frontier_size` climbing steadily means GC is lagging; lower BPS or increase GC cadence.
- `consensus_worker_queue.queued` growing indicates consensus backpressure; reduce tx rate or add CPU.
- `storage_mpt.cache_hit_rate_pct` low with high `db_loads` suggests increasing MPT cache sizes.

## Notes

- `TARGET_BPS` controls `dev_block_interval_ms`; you can also set `BLOCK_INTERVAL_MS` directly.
- `generate_tx` runs only in `dev`/`test` modes; the soak script sets `mode = "dev"` by default.
- `BOOTNODES` defaults to empty for isolated runs; set it if you want to join a network.
- `cache_hit_rate_pct` is derived from access vs DB-load counters in the MPT cache.
