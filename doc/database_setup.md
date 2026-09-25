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
| local grid, Europe envelope (**region-free**, see below), 60 min | 0.9–1.1 ms | index scan (`strikes_timestamp_geog`) |
| global grid, 10 min | ~10 ms | btree bitmap (`strikes_timestamp`) |
| global grid, 24 h (maximum service window) | 1152 ms (JIT 327 ms) | parallel btree bitmap + sort |
| histogram, `region=1`, 60 min | 1.6 ms | btree bitmap (`strikes_region_timestamp`) |
| `get_latest_time()` (no region) | < 0.5 ms | incremental sort on `strikes_timestamp` |
| `get_latest_time(region)` | < 0.3 ms | incremental sort on `strikes_region_timestamp` |
| `select` over 5 min (measured with the previous `ORDER BY id`, see note) | 0.5 ms | GiST bitmap (`strikes_timestamp_geog`) |

Note: the time-only `select` (`ORDER BY id`) picked the **GiST** index even
though there is no spatial predicate. It is not harmful at small intervals but
shows the multicolumn GiST competes with the plain btree for time-only scans.
`bo-db`'s default select now orders by `"timestamp", nanoseconds` (see
[Partitioning the `strikes` table](#partitioning-the-strikes-table)) instead of
`id`, so the plan/timing above must be re-validated against the new shape.

### Local-grid query and index layout

`get_local_strikes_grid` takes a `LocalGrid` envelope and **must not** add a
`region` predicate: `src/service.rs` passes `region = None` for local grids
(`run_grid_with_histogram`). The "local grid, `region=1`" row above was wrong:
on the Phase-1-only dataset `region` is `NULL`, so a `region=1` predicate
matches no rows at all and the 2.7 ms timing was an empty query, not a local
grid. Re-running the region-free query also shows that the index layout matters.

**8,000,000 rows, fresh statistics** (`data_area=5`, `x=2`, `y=9`,
`grid_base_length=5000`, 60-minute window, 18 result cells):

| Index layout | Plan | Time |
| --- | --- | --- |
| `strikes_geog` GiST (single column) + `strikes_timestamp` btree | `BitmapAnd` + in-memory sort | 0.14–0.35 s |
| `strikes_timestamp_geog` GiST (`("timestamp", geog)`, canonical) | index scan on `strikes_timestamp_geog` | **0.9–1.1 ms** |

**11,000,000 rows** (Phase 2: 3M Europe strikes in the last hour), same window,
306,808 strikes over 115,136 cells:

| Index layout | Statistics | Plan | Time |
| --- | --- | --- | --- |
| `strikes_geog` + `strikes_timestamp` | fresh | parallel `strikes_geog` bitmap + `HashAggregate` | 0.51–0.63 s |
| `strikes_geog` + `strikes_timestamp` | stale | `BitmapAnd` + `external merge` (est. 18 / actual 306,808) | 1.6–1.8 s |
| `strikes_timestamp_geog` (canonical) | fresh | parallel `strikes_timestamp_geog` bitmap + `HashAggregate` | 0.66–0.73 s |
| `strikes_timestamp_geog` (canonical) | stale | `Index Scan` on `strikes_timestamp_geog` + `external merge` (est. 25 / actual 306,808) | 1.1–1.3 s |

For a sparse window (8M, few strikes in the envelope) the multicolumn GiST is
about two orders of magnitude faster: it seeks by `"timestamp"` *and* `geog`
together, whereas the single-column `strikes_geog` scans every envelope entry
across all time and intersects it with the time index. For a dense window (11M
with 3M strikes in the hour) both layouts spend most of their time in the heap
and aggregate and the gap narrows. Under stale statistics the multicolumn GiST
degrades less because the single index still bounds the `"timestamp"` range.

Index sizes at 11M (one Phase 2 insert):

| Index | `strikes_geog` layout | canonical |
| --- | --- | --- |
| `strikes_timestamp_geog` | — | 1027 MB |
| `strikes_geog` | 928 MB | — |
| `strikes_region_timestamp` | 547 MB | 547 MB |
| `strikes_timestamp` | 390 MB | 390 MB |
| `strikes_pkey` | 300 MB | 300 MB |

### Stale statistics (the main risk)

Phase 2 inserted 3,000,000 rows in a single burst **without** an intervening
`ANALYZE` (equivalent to `autovacuum_analyze_scale_factor = 0.1` not having
fired yet on a large table). The region-free local-grid query above was then
run before and after `ANALYZE`:

| Index layout | Statistics | Plan | Estimate vs. actual | Time |
| --- | --- | --- | --- | --- |
| `strikes_geog` + `strikes_timestamp` | stale | `BitmapAnd` + `external merge` (7.8 MB to disk) | est. 18 / actual 306,808 | **1.6–1.8 s** |
| `strikes_geog` + `strikes_timestamp` | fresh | parallel `strikes_geog` bitmap + `HashAggregate` | est. 94,737 / actual 306,808 | **0.51–0.63 s** |
| `strikes_timestamp_geog` (canonical) | stale | `Index Scan` on `strikes_timestamp_geog` + `external merge` (7.8 MB to disk) | est. 25 / actual 306,808 | **1.1–1.3 s** |
| `strikes_timestamp_geog` (canonical) | fresh | parallel `strikes_timestamp_geog` bitmap + `HashAggregate` | est. 94,927 / actual 306,808 | **0.66–0.73 s** |

A ~3x regression for the `strikes_geog` layout and up to ~2x for the
multicolumn GiST, plus a disk spill, caused purely by stale statistics. On an
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

#### updated values

| Setting | Value |
| --- | --- |
| `random_page_cost` | 3.0 |
| `effective_cache_size` | 5GB |
| `shared_buffers` | 512MB |
| `work_mem` | 128MB |
| `effective_io_concurrency` | 50 |
| `max_wal_size` | 4GB |
| `jit` | off |

### 3. Application / schema follow-ups

- **Done:** the service now uses a `deadpool-postgres` connection pool
  (`src/postgres.rs`) sized from `db_connection_count`, so up to that many
  statements run in parallel instead of sharing one multiplexed socket.
- **Done:** statements are prepared with `prepare_cached`, so each SQL text is
  parsed and planned once per pooled connection (`deadpool-postgres` keeps the
  statement cache on the connection).
- **Done:** the no-op `ST_Transform(geog::geometry, 4326)` was replaced with a
  direct `geog::geometry` access in `strikes_query`, `grid_query` and
  `global_grid_query`. `select` / `select_key` keep the transform because
  `bo-db --srid` can request a different output SRID.
- **Keep the multicolumn `strikes_timestamp_geog` GiST.** A single-column
  `strikes_geog` GiST is not a drop-in replacement for the region-free
  local-grid query: for a sparse window it is ~100x slower (0.14–0.35 s vs
  0.9–1.1 ms at 8M) and under stale statistics ~1.5x slower (1.6–1.8 s vs
  1.1–1.3 s at 11M), even though it is ~100 MB smaller at 11M. See
  [Local-grid query and index layout](#local-grid-query-and-index-layout).
- **Done:** `strikes` is now declaratively RANGE-partitioned by `"timestamp"`
  (see [Partitioning the `strikes` table](#partitioning-the-strikes-table)), so
  retention is an O(1) partition `DROP` and autovacuum/analyze run per day.

## Partitioning the `strikes` table

The service never reads further back than 24 hours (`MAX_MINUTES_PER_DAY`,
`src/service.rs:37`), so the table is declaratively RANGE-partitioned by
`"timestamp"` with one partition per UTC day. Every service query carries a
`"timestamp"` predicate (`select`, grid, histogram) or an `ORDER BY
"timestamp"`, so partition pruning applies automatically; the importers
(`src/db.rs` `insert_many`) always supply the timestamp, so rows route to the
right partition.

The canonical definitions live in `tests/schema/strikes.sql`. Two consequences:

- The primary key is `(id, "timestamp")` — the partition key must be part of
  every unique constraint, so `id` alone can no longer be the primary key.
- `"timestamp"` is `NOT NULL` (the Rust insert already rejects a missing
  timestamp with `DbError::Column("timestamp")`).

### Maintenance

`tests/schema/strikes.sql` ships three functions:

| Function | Purpose |
| --- | --- |
| `strikes_create_partition(day date)` | create one UTC day's partition if missing |
| `strikes_ensure_partitions(ahead int = 2)` | create today + `ahead` future partitions |
| `strikes_drop_old_partitions(keep interval = '2 days')` | drop partitions older than `keep` |

Schedule them well before the day rolls over. `pg_cron` is the recommended
option because it runs inside the database and is unaffected by an
application/timer outage:

```sql
SELECT strikes_ensure_partitions(7);     -- e.g. every hour
SELECT strikes_drop_old_partitions();    -- e.g. once a day
```

Keep at least a two-day margin: `bo-import` / `bo-update` ingest ten-minute logs
with delay, and an insert whose timestamp falls in a dropped range has no
partition to land in. For the same reason **do not add a `DEFAULT` partition**
in production — every `CREATE ... PARTITION OF` would then have to scan the
default under `ACCESS EXCLUSIVE`.

#### Installing `pg_cron` on Ubuntu

`pg_cron` is not in the default Ubuntu repositories; install it from the
[PGDG](https://apt.postgresql.org/) repo so the package matches the server
version. The target is PostgreSQL 18 (`postgresql-18-cron`).

1. Configure the PGDG apt repo (skip if PostgreSQL was installed from it):

   ```bash
   sudo apt install -y curl ca-certificates
   sudo install -d /usr/share/postgresql-common/pgdg
   sudo curl -o /usr/share/postgresql-common/pgdg/apt.postgresql.org.asc \
     https://www.postgresql.org/media/keys/ACCC4CF8.asc
   . /etc/os-release
   echo "deb [signed-by=/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc] \
     https://apt.postgresql.org/pub/repos/apt $VERSION_CODENAME-pgdg main" \
     | sudo tee /etc/apt/sources.list.d/pgdg.list
   sudo apt update
   ```

2. Install the extension for the server major version:

   ```bash
   sudo apt install -y postgresql-18-cron
   ```

3. Preload the library and choose the database whose schema holds the `cron.*`
   objects. In Debian/Ubuntu the config file is
   `/etc/postgresql/18/main/postgresql.conf`:

   ```ini
   shared_preload_libraries = 'pg_cron'
   cron.database_name = 'postgres'
   ```

   `shared_preload_libraries` is mandatory — `pg_cron` starts a background
   worker and will not load otherwise.

4. Restart PostgreSQL. A reload is not sufficient because this is a preload
   library:

   ```bash
   sudo systemctl restart postgresql
   ```

5. Create the extension as a superuser, connected to `cron.database_name`:

   ```sql
   CREATE EXTENSION pg_cron;
   ```

#### Scheduling the maintenance jobs

Schedule the partition functions (adjust `ahead`/`keep` if the ingest delay
changes). Jobs run as the role that owns `cron.database_name` by default, so
either schedule them from a role that can execute the functions, or name the
target database explicitly with `cron.schedule_in_database`:

```sql
-- Keep a one-week lookahead; run hourly so a missed run cannot exhaust it.
SELECT cron.schedule(
    'strikes-ensure-partitions',
    '0 * * * *',
    $$SELECT strikes_ensure_partitions(7)$$);

-- Drop old data once a day, keeping the two-day backfill margin.
SELECT cron.schedule(
    'strikes-drop-old-partitions',
    '30 0 * * *',
    $$SELECT strikes_drop_old_partitions('2 days')$$);
```

Verify the schedule and inspect recent runs:

```sql
SELECT jobid, schedule, command, active FROM cron.job;

SELECT jobid, status, start_time, end_time, return_message
FROM cron.job_run_details
ORDER BY start_time DESC
LIMIT 10;
```

`pg_cron` records every run in `cron.job_run_details`, which grows unbounded;
prune it periodically (or add it to the drop job), e.g.:

```sql
DELETE FROM cron.job_run_details
WHERE end_time < now() - interval '7 days';
```

### Backfilling older data

`strikes_ensure_partitions` only creates **today and the next `ahead` days** — it
does not create past partitions. A backfill therefore needs a partition for
every UTC day it inserts into, otherwise PostgreSQL aborts the insert with

```
ERROR: no partition of relation "strikes" found for row
```

and `bo-import` retries the same (deterministic) failure. Before running
`bo-import --startdate <YYYYMMDD>`, create the partitions spanning the backfill
window up to today:

```sql
DO $$
DECLARE
    d date;
BEGIN
    FOR d IN
        SELECT generate_series('<YYYY-MM-DD>'::date,
                               (now() AT TIME ZONE 'UTC')::date,
                               interval '1 day')::date
    LOOP
        PERFORM strikes_create_partition(d);
    END LOOP;
END $$;
```

For example, on 2026-09-25 a `bo-import --startdate 20260924` needs at least the
`strikes_p20260924` partition (created automatically only if the schema was
applied on 2026-09-24 or 2026-09-25). Alternatively create the single day
directly:

```sql
SELECT strikes_create_partition('2026-09-24'::date);
```

Do not lower `strikes_drop_old_partitions`'s `keep` interval below the backfill
window, or the partition can be dropped mid-import.

### Converting an existing database

`tests/schema/strikes.sql` is a fresh-install schema; re-applying it to an
existing plain `strikes` table is not a migration. There are two options.

#### Option A — drop and recreate (recommended here)

The service only serves the last 24 hours and the ten-minute logs are
re-importable from `data.blitzortung.org`, so the current rows do not need to be
preserved. Dropping the table also drops its owned `strikes_id_seq`, so the new
`bigserial` starts cleanly:

```sql
DROP TABLE IF EXISTS strikes;
-- then apply tests/schema/strikes.sql
```

That creates the partitioned parent, the indexes, the maintenance functions and
today's partitions. The canonical schema has no dependent views or foreign keys,
so nothing else needs recreating. Restart the importers afterwards; backfill
recent history with `bo-import --startdate <date>` if desired (create the
partitions for the backfill window first — see
[Backfilling older data](#backfilling-older-data)).

#### Option B — convert in place (when the rows must be kept)

Short write pause, temporarily doubles disk:

```sql
-- 0. Make the maintenance functions available first: apply the
--    "Partition maintenance" block of tests/schema/strikes.sql.

BEGIN;

-- 1. Move the old table aside.  PostgreSQL keeps the index names when a table
--    is renamed, and index names are schema-unique, so free them before the
--    new parent creates its own indexes.
ALTER TABLE strikes RENAME TO strikes_old;
ALTER TABLE strikes_old RENAME CONSTRAINT strikes_pkey TO strikes_old_pkey;
ALTER INDEX strikes_timestamp        RENAME TO strikes_old_timestamp;
ALTER INDEX strikes_region_timestamp RENAME TO strikes_old_region_timestamp;
ALTER INDEX strikes_timestamp_geog   RENAME TO strikes_old_timestamp_geog;

-- 2. Parent.  Reuse the existing sequence: `bigserial` would try to create
--    `strikes_id_seq` again and fail.
CREATE TABLE strikes (
    id            bigint      NOT NULL DEFAULT nextval('strikes_id_seq'),
    "timestamp"   timestamptz NOT NULL,
    nanoseconds   SMALLINT,
    geog          GEOGRAPHY(Point),
    altitude      SMALLINT,
    region        SMALLINT,
    amplitude     REAL,
    error2d       SMALLINT,
    stationcount  SMALLINT,
    CONSTRAINT strikes_pkey PRIMARY KEY (id, "timestamp")
) PARTITION BY RANGE ("timestamp");
ALTER SEQUENCE strikes_id_seq OWNED BY strikes.id;

CREATE INDEX strikes_timestamp        ON strikes ("timestamp");
CREATE INDEX strikes_region_timestamp ON strikes (region, "timestamp");
CREATE INDEX strikes_timestamp_geog   ON strikes USING gist ("timestamp", geog);

-- 3. Create a partition per day spanned by the old rows, plus two future days.
DO $$
DECLARE
    d  date;
    lo date;
    hi date;
BEGIN
    SELECT min("timestamp")::date, max("timestamp")::date + 2 INTO lo, hi FROM strikes_old;
    FOR d IN SELECT generate_series(lo::timestamp, hi::timestamp, interval '1 day')::date LOOP
        PERFORM strikes_create_partition(d);
    END LOOP;
END $$;

-- 4. Copy the rows, then fix the sequence.
INSERT INTO strikes (id, "timestamp", nanoseconds, geog, altitude, region, amplitude, error2d, stationcount)
SELECT              id, "timestamp", nanoseconds, geog, altitude, region, amplitude, error2d, stationcount
FROM strikes_old;
SELECT setval('strikes_id_seq', GREATEST((SELECT max(id) FROM strikes), 1));

COMMIT;
DROP TABLE strikes_old;
```

For a very large table, replace steps 2–4 with a zero-copy `ATTACH PARTITION`:
fix the old table's primary key to `(id, "timestamp")`, add a matching
`CHECK ("timestamp" >= .. AND < ..)` so the attach does not rescan, rename the
old table *and its `strikes_pkey` constraint* (index names collide with the new
parent's), create the parent, then `ALTER TABLE strikes ATTACH PARTITION
strikes_pYYYYMMDD FOR VALUES FROM (..) TO (..)`.