# Tasks

## 1. Servicelog entry and row format (`src/service_log.rs`)

- [x] 1.1 Add a `query_type` field to `ServiceLogEntry`; set `G` in `global_entry` / `region_entry` / `local_entry`. Verify: `cargo test -p blitzortung-srv service_log` compiles and existing tests pass.
- [x] 1.2 Add `global_cluster_entry` / `local_cluster_entry` constructors (query type `C`, `minute_length = interval_count * 10`, grid-only fields as placeholders, `region` 0 / -1, local `x`/`y`/`data_area` for the local flavour). Verify: new unit tests assert the constructed entries' fields.
- [x] 1.3 Render `query_type` as the 6th logical field in `build_row` (15 fields total). Verify: updated `build_row` tests assert the exact row string with `G` at position 6 and 15 logical fields after dropping empty tab segments.
- [x] 1.4 Add a `type=G|C` tag to `access_metric_key`. Verify: updated `access_metric_key` tests assert the tag for grid and cluster entries.

## 2. Record cluster requests (`src/service.rs`)

- [x] 2.1 Call `record_usage(global_cluster_entry(...))` in `jsonrpc_get_global_clusters` after a successful response (blocked requests return before). Verify: a service test with a spawned usage-log consumer shows one `C` row in `servicelog_YYYY-MM-DD` with `minute_length = interval_count * 10`, `region = 0`, `-` grid-only fields.
- [x] 2.2 Call `record_usage(local_cluster_entry(...))` in `jsonrpc_get_local_clusters` likewise. Verify: service test shows one `C` row with `region = -1` and the request's `x`/`y`/`data_area`; the blocked-request test asserts no row is written.

## 3. Stats parser (`src/stats/parse.rs`)

- [x] 3.1 Bump `SERVICELOG_FIELDS` to 15, parse `query_type` at index 5, and shift the remaining indices. Verify: parser unit tests parse a 15-field row and reject 14-field rows.
- [x] 3.2 Accept `-` in `grid_baselength`, `count_threshold` and `fill` (fill becomes `Option<f64>`) so cluster rows parse. Verify: parser tests parse a cluster row with `-` placeholders; existing grid-row tests still pass.

## 4. Documentation

- [x] 4.1 Update the README servicelog format table to 15 fields with the `query_type` column at position 6 and the cluster row mapping. Verify: table matches `build_row` output in the service_log tests.
- [x] 4.2 Update `doc/service_setup.md` (usage-log description) and `doc/jsonrpc_methods.md` (`get_global_clusters` / `get_local_clusters` are recorded in the servicelog). Verify: docs mention `G`/`C` and `interval_count * 10`.

## 5. Integration verification

- [x] 5.1 Run `cargo test -p blitzortung-srv` (full suite, excluding `db-integration` which needs Docker) and verify all tests pass, including the new servicelog, service, and stats tests.
- [x] 5.2 Run `openspec validate --change servicelog-query-type --strict` and verify the change validates.
