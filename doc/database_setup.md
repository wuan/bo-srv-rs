# Database Setup

## Used versions

- PostgreSQL 18.6
- PostGIS 3.6.4

## Preliminary performance check

> **Status: preliminary.** The numbers below were produced on a local,
> single-node Docker instance with a synthetic dataset, not on the production
> database. They are intended to prioritise follow-up work and must be
> re-verified against production data before any change is made.

### Verification database version

The closest published multi-arch PostGIS image at the time of testing was used:

| | Target (this document) | Tested image |
| --- | --- | --- |
| PostgreSQL | 18.6 | **18.1** |
| PostGIS | 3.6.4 | **3.6.1** |
| Image | — | `imresamu/postgis:18-3.6` |

No `18.6` / `3.6.4` image was available for `linux/arm64`. The PostgreSQL
**major** version matches; the patch levels do not. Re-run the check against
the production build before acting on the settings below.

### Environment

- Apple Silicon host, PostgreSQL 18.1 in Docker (`imresamu/postgis:18-3.6`).
- Server settings taken from the project's production `postgresql.conf`
  where relevant: `shared_buffers = 512MB`, `work_mem = 4MB`,
  `maintenance_work_mem = 256MB`, `max_wal_size = 1GB`; all other values left
  at their defaults.
- Schema applied verbatim from `tests/schema/strikes.sql` (the four production
  indexes).
- Data generated with `generate_series`:
  - Phase 1: **8,000,000** strikes spread uniformly over the last 90 days
    (global lon/lat), then `ANALYZE`.
  - Phase 2: **3,000,000** additional strikes concentrated in the last hour
    over Europe (to exercise stale statistics), total **11,000,000**.
- Query shapes are the exact SQL produced by `src/query.rs`
  (grid / global grid / histogram / get_latest_time / select).

### Index sizes (8,000,000 rows)

| Relation | Size |
| --- | --- |
| `strikes` (heap) | 710 MB |
| all indexes | 1333 MB |
| `strikes_timestamp_geog` (GiST) | 675 MB |
| `strikes_region_timestamp` (btree) | 299 MB |
| `strikes_timestamp` (btree) | 188 MB |
| `strikes_pkey` (btree) | 171 MB |

The multicolumn GiST index is by far the largest single object (roughly
48% of all index bytes).

### Query timings (8,000,000 rows, fresh statistics)

| Query (service shape) | Time | Plan |
| --- | --- | --- |
| local grid, `region=1` + Europe envelope, 60 min | 2.7 ms | GiST bitmap (`strikes_timestamp_geog`) |
| global grid, 10 min | ~10 ms | btree bitmap (`strikes_timestamp`) |
| global grid, 24 h (maximum service window) | 1152 ms (JIT 327 ms) | parallel btree bitmap + sort |
| histogram, `region=1`, 60 min | 1.6 ms | btree bitmap (`strikes_region_timestamp`) |
| `get_latest_time()` (no region) | < 0.5 ms | incremental sort on `strikes_timestamp` |
| `get_latest_time(region)` | < 0.3 ms | incremental sort on `strikes_region_timestamp` |
| `select` over 5 min, `ORDER BY id` | 0.5 ms | GiST bitmap (`strikes_timestamp_geog`) |

Note: the time-only `select` (`ORDER BY id`) picked the **GiST** index even
though there is no spatial predicate. It is not harmful at small intervals but
shows the multicolumn GiST competes with the plain btree for time-only scans.

### Stale statistics (the main risk)

Phase 2 inserted 3,000,000 rows in a single burst **without** an intervening
`ANALYZE` (equivalent to `autovacuum_analyze_scale_factor = 0.1` not having
fired yet on a large table). The local-grid query was then run before and after
`ANALYZE`:

| Statistics | Plan | Estimate vs. actual | Time |
| --- | --- | --- | --- |
| stale | serial GiST bitmap + `external merge` sort (3.3 MB to disk) | est. 75 / actual 899,570 | **816 ms** |
| fresh (after `ANALYZE`) | parallel `strikes_region_timestamp` bitmap + in-memory sort | est. 409,850 / actual 423,321 | **149 ms** |

A 5.5x regression caused purely by stale statistics, plus a disk spill. On an
append-only table that grows large, the default `10%` analyze threshold is a
latent source of second-long grid queries under load.

### JIT overhead

The 24 h global grid exceeds `jit_above_cost` (default 100000), so PostgreSQL
JIT-compiles the geospatial row expressions:

| | Execution time | JIT compile |
| --- | --- | --- |
| JIT on | 1692–1755 ms | 229–312 ms |
| JIT off | 1619–1667 ms | — |

The compile time (dominated by `Inlining`, 125–200 ms per execution) is pure
added latency for a webservice. Typical 10-minute windows stay below
`jit_above_cost` and do not trigger JIT.

### Observed PG18 defaults

For reference, on this image the relevant defaults were
`random_page_cost = 4`, `effective_cache_size = 4GB`,
`max_parallel_workers_per_gather = 2`, and `effective_io_concurrency = 16`
(PG18 raises this default compared with older releases).

## Recommended changes (preliminary)

### 1. Per-table autovacuum for `strikes` (highest priority)

Prevents the stale-statistics regression above:

```sql
ALTER TABLE strikes SET (
  autovacuum_analyze_scale_factor      = 0.0,
  autovacuum_analyze_threshold         = 10000,
  autovacuum_vacuum_scale_factor       = 0.01,
  autovacuum_vacuum_insert_scale_factor = 0.01,
  autovacuum_vacuum_insert_threshold   = 10000,
  autovacuum_vacuum_cost_delay         = 0
);
```

Also review `autovacuum_max_workers` (default 3) so a large `strikes` table
does not starve the other databases.

### 2. Server settings, given an SSD

| Setting | Current | Suggested |
| --- | --- | --- |
| `random_page_cost` | 4.0 | 1.1 |
| `effective_cache_size` | 4GB | ~50–75% of RAM |
| `shared_buffers` | 512MB | ~25% of RAM |
| `work_mem` | 4MB | 64–256MB |
| `effective_io_concurrency` | 16 | 200 |
| `max_wal_size` | 1GB | 4–8GB |
| `jit` | on | off (or raise `jit_above_cost`) |

Observability, currently all off: `shared_preload_libraries =
'pg_stat_statements'`, `track_io_timing = on`, `log_min_duration_statement =
500ms`, `log_lock_waits = on`.

### 3. Application / schema follow-ups

- **Done:** the service now uses a `deadpool-postgres` connection pool
  (`src/postgres.rs`) sized from `db_connection_count`, so up to that many
  statements run in parallel instead of sharing one multiplexed socket.
- **No prepared-statement reuse.** `client.query(...)` re-plans every request;
  use `prepare_cached` / a statement cache.
- **Done:** the no-op `ST_Transform(geog::geometry, 4326)` was replaced with a
  direct `geog::geometry` access in `strikes_query`, `grid_query` and
  `global_grid_query`. `select` / `select_key` keep the transform because
  `bo-db --srid` can request a different output SRID.
- For a multi-hundred-million-row append-only table, consider **time
  partitioning** so autovacuum/analyze stay cheap and old data can be dropped.