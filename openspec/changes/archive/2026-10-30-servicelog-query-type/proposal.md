# Proposal

## Why

The servicelog (`servicelog_YYYY-MM-DD`) records only grid requests today:
`get_strikes_grid` / `get_global_strikes_grid` / `get_local_strikes_grid`.
The cluster endpoints (`get_global_clusters` / `get_local_clusters`) are never
logged, so usage statistics silently miss an entire class of client queries.
There is also no way to tell from a log row which kind of query produced it.

## What Changes

- The servicelog row gains a `query_type` column at position 6 (between
  `version` and `minute_offset`): `G` for grid requests, `C` for cluster
  requests. The row grows from 14 to 15 logical fields.
- `get_global_clusters` and `get_local_clusters` are recorded in the
  servicelog, like the grid endpoints (successful requests only; blocked
  requests stay unrecorded).
- For cluster rows, `minute_length` is logged as `interval_count * 10`
  (clusters are stored in 10-minute detection intervals), so the column keeps
  its grid semantics of "total minutes covered". Grid-only fields
  (`grid_baselength`, `count_threshold`, `fill`) are logged as `-` for cluster
  rows; `region` reuses `0` for global clusters and `-1` for local clusters.
- The StatsD access metric key gains a `type=G|C` tag.
- **BREAKING**: the servicelog row format changes from 14 to 15 fields and
  existing grid rows gain the `G` marker. `bo-servicelog-stats` is updated in
  the same change; no backwards compatibility with old 14-field rows is kept.

## Capabilities

### New Capabilities

- `servicelog`: the per-request usage-log row format and which JSON-RPC
  methods are recorded in it, including the query-type column and cluster
  request logging.

### Modified Capabilities

- (none — `client-user-agent` behaviour is unchanged; the platform/version
  columns keep their semantics)

## Impact

- `src/service_log.rs`: `ServiceLogEntry` gains a query type, new cluster
  entry constructors, `build_row` renders the new column, `access_metric_key`
  gains the type tag.
- `src/service.rs`: `jsonrpc_get_global_clusters` / `jsonrpc_get_local_clusters`
  call `record_usage`.
- `src/stats/parse.rs`: field count 14 -> 15, `query_type` parsed at index 5,
  numeric grid-only fields accept `-`.
- `README.md`, `doc/service_setup.md`, `doc/jsonrpc_methods.md`: row-format
  table and method documentation.
- Tests in `src/service_log.rs`, `src/stats/`, and `src/service.rs`.
