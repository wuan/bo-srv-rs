# Design

## Context

The servicelog pipeline is: `jsonrpc_*` handler builds a `ServiceLogEntry` ->
`record_usage` -> bounded `try_send` queue -> background consumer thread ->
GeoIP enrichment + `build_row` -> append to `servicelog_YYYY-MM-DD`. Only the
three grid handlers call `record_usage` today; the two cluster handlers
(`jsonrpc_get_global_clusters`, `jsonrpc_get_local_clusters`) never log. The
row has 14 logical tab-separated fields; `city` is tab-padded, so consumers
split on `'\t'` and drop empty segments. `bo-servicelog-stats`
(`src/stats/parse.rs`) is the only consumer and expects exactly 14 fields.
See proposal.md for motivation and `specs/servicelog/spec.md` for the
requirements.

## Goals / Non-Goals

**Goals:**

- Add a `query_type` column (`G`/`C`) at logical position 6 of every row.
- Record cluster requests in the servicelog with their parameters mapped onto
  the existing columns.
- Keep the single-consumer parser in sync in the same change.

**Non-Goals:**

- Backwards compatibility with 14-field rows (decided: clean break, the stats
  tool is updated in the same change).
- A separate `interval_count` column (decided: cluster rows log
  `minute_length = interval_count * 10` instead).
- Changing what blocks a request or the user-agent/platform behaviour
  (`client-user-agent` spec is untouched).

## Decisions

**Query type as column 6, not appended at the end.** Placing it next to the
other request-identity fields (platform/version) keeps the row readable and
groups "what was asked" together; appending after `fill` would bury it. Since
backwards compatibility is out of scope, shifting the indices of columns 6-14
is acceptable and the parser is updated in lockstep.

**Query type carried on `ServiceLogEntry`, not passed to `build_row`.** The
entry already carries everything `build_row` needs; adding a `query_type`
field set by the existing grid constructors (`G`) and two new cluster
constructors (`C`) keeps `build_row`'s signature stable and makes the row
self-describing. Alternative: a `build_row` parameter — rejected because every
call site would have to pass it and the entry is the natural owner.

**Cluster `minute_length` logged as `interval_count * 10`.** Cluster producers
store clusters in 10-minute detection intervals (`interval_seconds = 600`),
so `interval_count * 10` is the total minutes the request covers. This keeps
the column's grid semantics ("window length in minutes"), which the stats
aggregations rely on. The raw per-interval `minute_length` parameter is not
logged. Alternative: a new `interval_count` column — rejected to keep the row
at 15 fields.

**Grid-only fields are `-` on cluster rows; `region` reused as `0`/`-1`.**
`grid_baselength`, `count_threshold` and `fill` have no cluster meaning, so
they use the established `-` placeholder. `region` reuses `0` (global) and
`-1` (local) so `ServiceLogRow::is_global()` / `is_local()` keep working and
local cluster rows show up in the stats world-map overlay, distinguishable by
the `C` type. Alternative: `-` for `region` on cluster rows — rejected because
it would make local/global indistinguishable without parsing the type.

**Parser accepts `-` in the numeric grid-only fields.** Today
`fill: fields[13].parse().ok()?` would reject `-` and silently skip the whole
cluster row as malformed. The parser generalises the existing
`dash_to_int` handling to `grid_baselength`, `count_threshold` and `fill`
(`Option<f64>` for fill), and `SERVICELOG_FIELDS` becomes 15.

**Metric tag `type=G|C` on the `access` key.** `access_metric_key` already tags
version/region/minutes/offset/grid; adding the type lets StatsD split grid vs
cluster traffic without parsing the log file.

## Risks / Trade-offs

- [Risk] Hand-edited or external consumers of the 14-field format break.
  -> Mitigation: none needed by decision (clean break); `bo-servicelog-stats` is
  updated in the same change and is the only known consumer.
- [Risk] Local cluster rows appear in the stats world-map overlay alongside
  local grid rows. -> Accepted deliberately; the `C` type column lets reports
  filter them.
- [Risk] `interval_count * 10` assumes 10-minute cluster intervals; a producer
  storing other interval lengths would make the logged `minute_length`
  inaccurate. -> Accepted; 10 minutes is the canonical `bo-cluster` interval
  and the spec pins the formula.

## Migration Plan

No runtime migration: the service and `bo-servicelog-stats` ship together in
one release, and old log files are simply not parsed by the new tool
(15-field parser rejects 14-field rows as malformed). Rollback is the previous
release; already-written old rows stay readable by the old tool.
