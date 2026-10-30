# Spec Delta

## Purpose

The servicelog (`servicelog_YYYY-MM-DD` in the configured usage-log directory)
records one tab-separated row per successful data request, carrying the request
time, GeoIP location, client platform/version, and the query parameters, so
usage can be analysed per day with `bo-servicelog-stats`.

## ADDED Requirements

### Requirement: Every servicelog row carries a query type at column 6

The system SHALL write a `query_type` field as the 6th logical column of every
servicelog row, between `version` and `minute_offset`: `G` for a grid request
(`get_strikes_grid`, `get_global_strikes_grid`, `get_local_strikes_grid`) and
`C` for a cluster request (`get_global_clusters`, `get_local_clusters`). The
row SHALL have 15 logical fields in total.

#### Scenario: Grid request is logged with G

- **WHEN** a successful `get_strikes_grid` request is written to the servicelog
- **THEN** the 6th logical field of the row is `G`

#### Scenario: Global cluster request is logged with C

- **WHEN** a successful `get_global_clusters` request is written to the servicelog
- **THEN** the 6th logical field of the row is `C`

#### Scenario: Local cluster request is logged with C

- **WHEN** a successful `get_local_clusters` request is written to the servicelog
- **THEN** the 6th logical field of the row is `C`

### Requirement: Cluster requests are recorded in the servicelog

The system SHALL record every successful `get_global_clusters` and
`get_local_clusters` request in the servicelog, exactly like the grid
endpoints. Blocked or invalid cluster requests SHALL NOT be recorded.

#### Scenario: Successful global cluster request is recorded

- **WHEN** a `get_global_clusters` request passes the client checks and is served
- **THEN** one servicelog row with query type `C` is appended for it

#### Scenario: Successful local cluster request is recorded

- **WHEN** a `get_local_clusters` request passes the client checks and is served
- **THEN** one servicelog row with query type `C` carrying its `x`/`y`/`data_area` is appended

#### Scenario: Blocked cluster request is not recorded

- **WHEN** a `get_global_clusters` or `get_local_clusters` request is blocked (invalid user agent, blocked IP, bad content type, referer)
- **THEN** no servicelog row is written for it

### Requirement: Cluster rows map their parameters onto the grid columns

For a cluster row the system SHALL write `minute_length` as
`interval_count * 10` (the total minutes covered by the requested 10-minute
cluster intervals), SHALL write `grid_baselength`, `count_threshold` and
`fill` as `-`, SHALL write `region` as `0` for a global cluster request and
`-1` for a local cluster request, and SHALL write the local `x`/`y`/`data_area`
for a local cluster request and `-` for a global one. `minute_offset` SHALL be
the request's `minute_offset`.

#### Scenario: Global cluster row records interval_count * 10 as minute_length

- **WHEN** a `get_global_clusters` request with `interval_count = 6` is logged
- **THEN** the row's query type is `C`, `minute_length` is `60`, `grid_baselength` is `-`, `region` is `0`, `count_threshold` is `-`, `x`/`y`/`data_area` are `-`, and `fill` is `-`

#### Scenario: Local cluster row keeps its tile coordinates

- **WHEN** a `get_local_clusters` request with `x = 101`, `y = 202`, `data_area = 5`, `interval_count = 1` is logged
- **THEN** the row's query type is `C`, `minute_length` is `10`, `region` is `-1`, and `x`/`y`/`data_area` are `101`/`202`/`5`

### Requirement: The access metric key tags the query type

The system SHALL include a `type=G` or `type=C` tag in the StatsD `access` metric
key emitted for each servicelog entry, so grid and cluster requests can be
distinguished in metrics.

#### Scenario: Grid entry metric carries type=G

- **WHEN** the access metric key is built for a grid servicelog entry
- **THEN** the key contains `type=G`

#### Scenario: Cluster entry metric carries type=C

- **WHEN** the access metric key is built for a cluster servicelog entry
- **THEN** the key contains `type=C`
