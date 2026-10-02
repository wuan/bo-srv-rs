# JSON-RPC methods

This document is the reference for the JSON-RPC methods served by
`bo-webservice`.  Every method is available over both transports (the default
HTTP/1.1 `POST /` / `GET ?request=` / JSONP and the opt-in LSP-style
`Content-Length` framing on a raw TCP socket); see the README for the wire
details.  The implementation lives in `src/jsonrpc.rs` (dispatch, parameter
resolution, envelope rendering) and `src/service.rs` (validation, caching and
response shaping).

The method set is ported from the Python `blitzortung` service
(`blitzortung/service/base.py` and friends); the two cluster methods are new
and read pre-computed clusters from the `strike_clusters` table.

## Contents

- [Request format](#request-format)
- [Response dialects](#response-dialects)
- [Parameter passing](#parameter-passing)
- [Error codes](#error-codes)
- [Common parameter concepts](#common-parameter-concepts)
- [Cluster time anchoring](#cluster-time-anchoring)
- [Validation rules](#validation-rules)
- [Method index](#method-index)
- [`check`](#check)
- [`get_strikes`](#get_strikes)
- [`get_strikes_grid` / `get_strikes_raster` / `get_strokes_raster`](#get_strikes_grid)
- [`get_global_strikes_grid`](#get_global_strikes_grid)
- [`get_local_strikes_grid`](#get_local_strikes_grid)
- [`get_global_clusters`](#get_global_clusters)
- [`get_local_clusters`](#get_local_clusters)
- [Result object reference](#result-object-reference)
- [Worked examples](#worked-examples)

## Request format

A request is a single JSON object (batches are **not** supported).  The
recognised members are:

| Member | Type | Required | Meaning |
| --- | --- | --- | --- |
| `method` | string | yes | The method name (see the [index](#method-index)). |
| `params` | array \| object \| null | no | The method arguments, positional (array) or named (object). |
| `id` | any JSON scalar | no | Correlates the response with the request. |
| `jsonrpc` | string | no | Selects the response dialect (`"2.0"` for JSON-RPC 2.0). |

```json
{"jsonrpc": "2.0", "method": "check", "params": [], "id": 1}
```

A non-object body (for example a batch array) is rejected with an
invalid-request fault, and an unparseable body yields a parse-error fault.

## Response dialects

The response envelope mirrors `txjsonrpc_ng` with `treat_zero_id_as_pre1 = True`
(matching the Android clients):

| Request | Success envelope | Fault envelope |
| --- | --- | --- |
| explicit `jsonrpc` field != 2 | v1 dict: `{"result": .., "error": null, "id": ..}` | `{"result": null, "error": {"fault": "Fault", "faultCode": .., "faultString": ..}, "id": ..}` |
| explicit `jsonrpc: "2.0"` | v2 dict: `{"jsonrpc": "2.0", "result": .., "id": ..}` | `{"jsonrpc": "2.0", "error": {"code": .., "message": .., "data": ""}, "id": ..}` |
| no version, id `0`/missing/`""`/`false` | pre-1.0 bare array `[result]` | `{"fault": "Fault", "faultCode": .., "faultString": ..}` |
| no version, truthy id | v1 dict (as above) | v1 dict (as above) |

An explicit version field wins over the id: `jsonrpc: "1.0"` renders the v1
dict even for id `0`, and any version other than 2 renders the v1 shape.  A
non-numeric version field (or `inf`/`nan`) yields a pre-1.0
invalid-request fault.

The service always answers, even for id-less requests.  Faults raised *before*
id/version selection (bad `params` type, missing `method`) are rendered as bare
pre-1.0 fault dicts regardless of the request dialect.

## Parameter passing

`params` may be omitted, `null`, an empty array, a positional array, or an
object keyed by parameter name:

```json
{"method": "get_strikes_grid", "params": [60, 10000, 0, 1, 0], "id": 1}
{"method": "get_strikes_grid",
 "params": {"minute_length": 60, "grid_base_length": 10000, "region": 1},
 "id": 1}
```

- Positional arguments map by index; missing trailing arguments take their
  defaults.
- Named arguments map by name; unknown names are ignored and missing optional
  names take their defaults.
- A **missing required** argument (neither present positionally nor by name)
  raises the Python `TypeError` analogue and is answered with the generic
  `FAILURE` code `8002` and the message
  `missing 1 required positional argument: '<name>'`.
- A required argument that is **present but `null`** is *not* a fault.  It
  reaches the service's integer coercion, which rejects it and returns the
  blocked `{}` result (see [Validation rules](#validation-rules)).
- Any other `params` type (a number, string, ...) is answered with
  `-32602 Invalid params: expected an array or object` as a bare pre-1.0 fault.

## Error codes

| Code | Constant | When |
| --- | --- | --- |
| `-32600` | `INVALID_REQUEST` | Unparseable body, non-object body, missing `method`, invalid `jsonrpc` version field. |
| `-32601` | `METHOD_NOT_FOUND` | Unknown `method` (`function <name> not found`). |
| `-32602` | `INVALID_PARAMS` | `params` is not an array/object/`null`. |
| `8002` | `FAILURE` | Missing required argument (Python `TypeError` analogue) or a server/database error while producing the result. |

## Common parameter concepts

### `minute_length` and `minute_offset`

`minute_length` is the length of the detection window in minutes;
`minute_offset` shifts the window into the past.  The interval used by the grid
endpoints is:

```text
end   = now (UTC) + minute_offset minutes
start = end - minute_length minutes
```

The cluster endpoints use the same formula but truncate `now` down to the whole
minute first (they match stored cluster timestamps exactly):

```text
end   = floor(now to the minute) + minute_offset minutes
start = end - minute_length minutes
```

Both are clamped by `TimeConstraint.enforce`:

- `minute_length` is clamped to `[0, 1440]`; a clamped-to-`0` value becomes the
  default `60`.
- `minute_offset` is clamped to `[-(1440 - minute_length), 0]` (so the window
  never extends past "now" and never reaches more than a day back).

### `minute_length` defaults per method

| Method | Required? | Default |
| --- | --- | --- |
| `get_strikes` | yes | — |
| `get_strikes_grid` + raster aliases | yes | — |
| `get_global_strikes_grid` | yes | — |
| `get_local_strikes_grid` | no | `60` |
| `get_global_clusters` | yes | — |
| `get_local_clusters` | no | `60` |

### `grid_base_length`

The requested grid cell size / UTM grid baseline in metres.  It is validated on
the **unclamped** value and then clamped to the endpoint minimum:

- Region and local endpoints: minimum `5000`.
- Global endpoint: minimum `25000`.

Valid sizes are `5000, 10000, 25000, 50000, 100000`; any other value blocks the
request (see [Validation rules](#validation-rules)).  Defaults to `10000`
everywhere.  The requested (pre-clamp) value is what the usage log records.

### `region`

For `get_strikes_grid` (and the raster aliases) this selects one of the seven
fixed UTM region grids.  It is clamped to `[1, 7]`.  The `get_global_strikes_grid`
endpoint has no `region` argument (it always covers the whole world).

| Region | Lon range | Lat range | UTM zone | Area |
| --- | --- | --- | --- | --- |
| 1 | -25 .. 57 | 27 .. 72 | 33N | Europe |
| 2 | 110 .. 180 | -50 .. 0 | 55S | Oceania |
| 3 | -140 .. -50 | 10 .. 60 | 14N | North America |
| 4 | 85 .. 150 | -10 .. 60 | 50N | Asia |
| 5 | -100 .. -30 | -50 .. 20 | 20S | South America |
| 6 | -20 .. 50 | -40 .. 40 | 33N | Africa |
| 7 | -115 .. -50 | 0 .. 30 | 14N | Central America |

An out-of-range region is clamped into `[1, 7]` (it is not blocked).

### `count_threshold`

Integer minimum for the per-cell strike count.  Negative values are clamped to
`0`, and `0` (the default) applies no filter.  A value of `n > 0` keeps only
cells whose count satisfies `count(*) > n` (strictly greater).

### `x`, `y`, `data_area` (local endpoints)

The local grid is a `3 * data_area`-degree neighbourhood grid, anchored at a
tile:

```text
reference_longitude = (x - 1) * data_area
reference_latitude  = (y - 1) * data_area
grid size           = data_area * 3 degrees
```

`x` and `y` identify the tile (`x` grows eastward, `y` northward) and
`data_area` is the tile size in degrees.  `data_area` is clamped with
`max(5, data_area)` (the default is `5`).  The local cluster endpoint reuses the
same envelope as the geometry filter for the stored clusters.

### `interval_count` (clusters)

The number of detection intervals to return.  Clamped with `max(1, ..)`
(default `1`).  The newest interval ends at the (minute-truncated) interval
`end`; each of the `interval_count - 1` earlier intervals steps back by
`minute_length` minutes.

Because the cluster producers only store a cluster for the minutes they actually
ran, the newest interval **anchor** is snapped to the most recent stored cluster
timestamp in `[end - lookback, end]` (see
[Cluster time anchoring](#cluster-time-anchoring)); when no cluster matches, the
requested `end` is kept.  The response's `t` reports the snapped interval end.

### Cluster time anchoring

The two cluster methods return **stored** clusters, so the interval end must
match a stored `strike_clusters."timestamp"` exactly.  The service resolves the
newest interval as follows:

1. Compute the requested `end = floor(now to the minute) + minute_offset`.
2. Look for the newest stored cluster timestamp `within [end - lookback, end]`
   that has `interval_seconds = minute_length * 60` (and, for the local
   endpoint, intersects the tile's neighbourhood envelope).
3. Use that timestamp as the anchor when found, otherwise keep the requested
   `end`.
4. Build the `interval_count` timestamps by stepping back `minute_length`
   minutes from the anchor.

`lookback = minute_length * interval_count`, capped at one day.  This means a
request that asks for "the last hour at 10-minute steps" still returns the
latest available series when the current minute has no cluster: at `14:32:12`
with `minute_length = 10, interval_count = 6` the timestamps are

```text
14:32:00, 14:22:00, 14:12:00, 14:02:00, 13:52:00, 13:42:00
```

when a cluster is stored at `14:32`, but fall back to

```text
14:31:00, 14:21:00, 14:11:00, 14:01:00, 13:51:00, 13:41:00
```

when the newest stored cluster is at `14:31` (and to `14:30, 14:20, ...` when it
is at `14:30`).  The local endpoint applies the same area filter to the anchor
lookup, so a tile snaps to the latest cluster within its own neighbourhood.

## Validation rules

Before a data request reaches the grid/cluster producer,
`base.py`'s validation order is reproduced:

1. **`__to_int` coercion.** Every numeric argument is coerced with Python
   `int()` semantics:
   - booleans → `1`/`0`;
   - floats truncate toward zero;
   - strings are parsed as integers (surrounding whitespace is ignored);
   - anything else — including `null` — fails the coercion.

   If **any** argument fails the coercion, the request is treated as blocked and
   the method returns the empty object `{}` (not a fault).

2. **`is_forbidden` client checks.** A request is blocked (returns `{}` and logs
   a `BLOCKED` access line) when any of the following holds:
   - the resolved client IP is in the forbidden set;
   - the `User-Agent` is not of the form `bo-android-<integer>`;
   - the `Content-Type` is not exactly `text/json`;
   - a non-empty `Referer` header is present.

   The resolved client is the first component of `X-Forwarded-For` when that
   header is present and non-empty, otherwise the peer IP.

3. **`grid_base_length` checks** (region/global/local grid endpoints only):
   the **unclamped** value must be at least the endpoint minimum (5000 for
   region/local, 25000 for global) and must be one of the
   [valid sizes](#grid_base_length).  Otherwise the request is blocked (`{}`).

4. **Clamping** after validation:
   - `grid_base_length = max(minimum, grid_base_length)`;
   - `minute_length`/`minute_offset` per
     [`TimeConstraint`](#minute_length-and-minute_offset);
   - `region` into `[1, 7]`;
   - `count_threshold = max(0, ..)`;
   - `data_area = max(5, ..)`;
   - `interval_count = max(1, ..)`.

A **blocked** request is not an error: the method returns `{}` in the normal
success envelope.  A **database failure** while producing a result is a fault
(`8002`).

### Response compression

Over the HTTP transport a rendered response is gzipped (with
`Content-Encoding: gzip`) when the request advertises `Accept-Encoding: gzip`
and the body is at least 1000 bytes.  Clients with a valid
`bo-android-<version>` where `1 <= version <= 177` have their `Accept-Encoding`
header stripped first (`fix_bad_accept_header`) and therefore never receive
gzip.

### Caching

Successful results are cached per resolved parameter tuple (short TTL 20 s,
long 60 s; local caps 100/400).  Concurrent requests for the same key share one
in-flight computation ("single-flight").  Cache hits still return the same
result shape; blocked results are computed on the fast path and are not cached.

## Method index

| Method | Required params | Optional params (default) | Result |
| --- | --- | --- | --- |
| [`check`](#check) | — | — | `{"count": n}` |
| [`get_strikes`](#get_strikes) | `minute_length` | `id_or_offset` (`0`) | `null` |
| [`get_strikes_grid`](#get_strikes_grid) | `minute_length` | `grid_base_length` (`10000`), `minute_offset` (`0`), `region` (`1`), `count_threshold` (`0`) | [grid object](#grid-object) |
| [`get_strikes_raster` / `get_strokes_raster`](#get_strikes_grid) | `minute_length` | `grid_base_length` (`10000`), `minute_offset` (`0`), `region` (`1`) | [grid object](#grid-object) |
| [`get_global_strikes_grid`](#get_global_strikes_grid) | `minute_length` | `grid_base_length` (`10000`), `minute_offset` (`0`), `count_threshold` (`0`) | [grid object](#grid-object) |
| [`get_local_strikes_grid`](#get_local_strikes_grid) | `x`, `y` | `grid_base_length` (`10000`), `minute_length` (`60`), `minute_offset` (`0`), `count_threshold` (`0`), `data_area` (`5`) | [grid object](#grid-object) |
| [`get_global_clusters`](#get_global_clusters) | `minute_length` | `minute_offset` (`0`), `interval_count` (`1`) | [cluster object](#cluster-object) |
| [`get_local_clusters`](#get_local_clusters) | `x`, `y` | `minute_length` (`60`), `minute_offset` (`0`), `data_area` (`5`), `interval_count` (`1`) | [cluster object](#cluster-object) |

The positional order shown is the array order; named `params` use the names in
the table.

---

## `check`

Liveness/echo endpoint.  Counts the number of `check` calls this process has
served (starting at `1`).  Takes no parameters.

**Result:** `{"count": <integer>}`

```json
{"jsonrpc": "2.0", "method": "check", "params": [], "id": 1}
{"jsonrpc": "2.0", "result": {"count": 1}, "id": 1}
```

No validation applies: any client may call it, with or without headers.

---

## `get_strikes`

Legacy endpoint that is **blocked for all requests** — it never queries the
database and always returns `null`.  It is retained for protocol compatibility
(and to log the client).

| # | Name | Required | Type | Default | Meaning |
| --- | --- | --- | --- | --- | --- |
| 1 | `minute_length` | yes | integer | — | Detection window length in minutes. |
| 2 | `id_or_offset` | no | integer | `0` | Legacy identifier/offset. |

**Result:** `null` (in the request's envelope dialect).

Both arguments are coerced with `__to_int`; a failed coercion also yields
`null`.  Unlike the data endpoints it performs no `is_forbidden` check and
returns no `{}`.

```json
{"jsonrpc": "2.0", "method": "get_strikes", "params": [60, 0], "id": 2}
{"jsonrpc": "2.0", "result": null, "id": 2}
```

---

## `get_strikes_grid`

Region-grid endpoint.  Returned as a grid of strike counts for one of the seven
fixed UTM regions.  `get_strikes_raster` and the legacy misspelling
`get_strokes_raster` are aliases that dispatch to the same handler; they accept
only the first four parameters (`count_threshold` is fixed to `0` for them).
`get_strikes_raster` and `get_strokes_raster` produce identical results to
`get_strikes_grid` with `count_threshold = 0`.

| # | Name | Required | Type | Default | Meaning |
| --- | --- | --- | --- | --- | --- |
| 1 | `minute_length` | yes | integer | — | Window length in minutes (clamped to `[0, 1440]`, `0` → `60`). |
| 2 | `grid_base_length` | no | integer | `10000` | Cell size / grid baseline in metres (≥ 5000, valid sizes only). |
| 3 | `minute_offset` | no | integer | `0` | Window offset in minutes (clamped). |
| 4 | `region` | no | integer | `1` | Region id, clamped to `[1, 7]`. |
| 5 | `count_threshold` | no | integer | `0` | Keep cells with `count(*) > n` (≥ 0). |

**Result:** a [grid object](#grid-object).  A blocked/invalid request returns
`{}`.

```json
{"jsonrpc": "2.0", "method": "get_strikes_grid",
 "params": [60, 10000, 0, 1, 0], "id": 3}
```

---

## `get_global_strikes_grid`

Whole-world grid endpoint (no region selection).  The returned rows use the
global y-axis convention (`ry = -ry - 1`).

| # | Name | Required | Type | Default | Meaning |
| --- | --- | --- | --- | --- | --- |
| 1 | `minute_length` | yes | integer | — | Window length in minutes (clamped to `[0, 1440]`, `0` → `60`). |
| 2 | `grid_base_length` | no | integer | `10000` | Cell size / grid baseline in metres (≥ **25000**, valid sizes only). |
| 3 | `minute_offset` | no | integer | `0` | Window offset in minutes (clamped). |
| 4 | `count_threshold` | no | integer | `0` | Keep cells with `count(*) > n` (≥ 0). |

**Result:** a [grid object](#grid-object).  A blocked/invalid request returns
`{}`.  Note the higher `grid_base_length` minimum than the region endpoint.

```json
{"jsonrpc": "2.0", "method": "get_global_strikes_grid",
 "params": [60, 25000, 0, 0], "id": 4}
```

---

## `get_local_strikes_grid`

Grid for the neighbourhood of a client tile.  The grid spans
`3 * data_area` degrees centred on the tile's `(x, y)` (see
[`x`, `y`, `data_area`](#x-y-data_area-local-endpoints)).

| # | Name | Required | Type | Default | Meaning |
| --- | --- | --- | --- | --- | --- |
| 1 | `x` | yes | integer | — | Tile x (grows eastward). |
| 2 | `y` | yes | integer | — | Tile y (grows northward). |
| 3 | `grid_base_length` | no | integer | `10000` | Cell size / grid baseline in metres (≥ 5000, valid sizes only). |
| 4 | `minute_length` | no | integer | `60` | Window length in minutes (clamped to `[0, 1440]`, `0` → `60`). |
| 5 | `minute_offset` | no | integer | `0` | Window offset in minutes (clamped). |
| 6 | `count_threshold` | no | integer | `0` | Keep cells with `count(*) > n` (≥ 0). |
| 7 | `data_area` | no | integer | `5` | Tile size in degrees, clamped with `max(5, ..)`. |

**Result:** a [grid object](#grid-object).  A blocked/invalid request returns
`{}`.  The local flavour also applies the region-grid x/y axis mapping (flip and
bound-check).

```json
{"jsonrpc": "2.0", "method": "get_local_strikes_grid",
 "params": [101, 202, 10000, 60, 0, 0, 5], "id": 5}
```

---

## `get_global_clusters`

Return **stored** clusters (from `strike_clusters`) for the whole world over the
requested intervals.  Unlike the grid endpoints this method does not cluster on
the fly, and it applies the client checks but **not** the `region` or
`grid_base_length` rules.

| # | Name | Required | Type | Default | Meaning |
| --- | --- | --- | --- | --- | --- |
| 1 | `minute_length` | yes | integer | — | Interval length in minutes (clamped to `[0, 1440]`, `0` → `60`). |
| 2 | `minute_offset` | no | integer | `0` | Interval offset in minutes (clamped). |
| 3 | `interval_count` | no | integer | `1` | Number of intervals, clamped with `max(1, ..)`. |

**Result:** a [cluster object](#cluster-object).  A blocked/invalid request
returns `{}`.

The newest interval ends at the minute-truncated `now + minute_offset`, snapped
to the latest stored cluster within the requested window (see
[Cluster time anchoring](#cluster-time-anchoring)); the `interval_count - 1`
earlier intervals step back by `minute_length` each.  A database failure
surfaces as a fault (`8002`).

```json
{"jsonrpc": "2.0", "method": "get_global_clusters",
 "params": [60, 0, 1], "id": 6}
```

---

## `get_local_clusters`

Stored clusters restricted to the local tile envelope.  Validation mirrors
[`get_global_clusters`](#get_global_clusters); the `LocalGrid { data_area, x, y }`
envelope (the same footprint as `get_local_strikes_grid`) is used as the
geometry filter.  `data_area` is clamped with `max(5, ..)`.

| # | Name | Required | Type | Default | Meaning |
| --- | --- | --- | --- | --- | --- |
| 1 | `x` | yes | integer | — | Tile x (grows eastward). |
| 2 | `y` | yes | integer | — | Tile y (grows northward). |
| 3 | `minute_length` | no | integer | `60` | Interval length in minutes (clamped to `[0, 1440]`, `0` → `60`). |
| 4 | `minute_offset` | no | integer | `0` | Interval offset in minutes (clamped). |
| 5 | `data_area` | no | integer | `5` | Tile size in degrees, clamped with `max(5, ..)`. |
| 6 | `interval_count` | no | integer | `1` | Number of intervals, clamped with `max(1, ..)`. |

**Result:** a [cluster object](#cluster-object).  A blocked/invalid request
returns `{}`.

The newest interval is snapped to the latest stored cluster **within the tile's
neighbourhood** (see [Cluster time anchoring](#cluster-time-anchoring)).

```json
{"jsonrpc": "2.0", "method": "get_local_clusters",
 "params": [101, 202, 60, 0, 5, 1], "id": 7}
```

---

## Result object reference

### Grid object

Returned by `get_strikes_grid`, `get_strikes_raster`, `get_strokes_raster`,
`get_global_strikes_grid` and `get_local_strikes_grid`, or `{}` when the request
was blocked/invalid.

| Key | Type | Meaning |
| --- | --- | --- |
| `r` | array | Strike rows `[rx, ry, count, age]`. |
| `xd` | number | Grid cell width in degrees (`%.6f`). |
| `yd` | number | Grid cell height in degrees (`%.6f`). |
| `x0` | number | Western grid edge longitude (`%.4f`). |
| `y1` | number | Northern grid edge latitude (`%.4f`). |
| `xc` | integer | Number of columns (x bins). |
| `yc` | integer | Number of rows (y bins). |
| `t` | string | Interval end as `%Y%m%dT%H:%M:%S` (UTC). |
| `dt` | integer | Interval length in seconds. |
| `h` | array | 5-minute histogram bins; empty when `minute_length <= 10`. |

Each row `[rx, ry, count, age]`:

- `rx`/`ry` are the cell indices in the returned (possibly flipped) grid space.
  Region/local grids flip the y axis (`ry = yc - ry`) and drop rows outside the
  bin range; the global grid uses `ry = -ry - 1`.
- `count` is the number of strikes in the cell.
- `age` is the (negative) age in **total seconds**:
  `-int((end_time - timestamp).total_seconds())`.

### Cluster object

Returned by `get_global_clusters` and `get_local_clusters`, or `{}` when the
request was blocked/invalid.

| Key | Type | Meaning |
| --- | --- | --- |
| `t` | string | Interval end as `%Y%m%dT%H:%M:%S` (UTC); the snapped anchor (see [Cluster time anchoring](#cluster-time-anchoring)). |
| `dt` | integer | Interval length in seconds (`minute_length * 60`). |
| `clusters` | array | The cluster objects (may be empty). |

Each cluster object:

| Key | Type | Meaning |
| --- | --- | --- |
| `id` | integer | Stored cluster id. |
| `timestamp` | string | The cluster interval end (`%Y%m%dT%H:%M:%S`). |
| `interval_seconds` | integer | The stored interval length in seconds. |
| `strike_count` | integer | Number of strikes in the cluster. |
| `area` | number | Cluster area. |
| `shape` | array | List of `[lon, lat]` pairs (empty when the stored shape is missing). |

> `dt` in the cluster response is `minute_length * 60`, i.e. the requested
> interval, while `interval_seconds` is the value stored with each cluster.  For
> a request whose `minute_length` matches the `bo-cluster` interval they are
> equal.

## Worked examples

### Positional, JSON-RPC 2.0

```json
{"jsonrpc": "2.0", "method": "get_strikes_grid",
 "params": [60, 10000, 0, 1, 0], "id": 42}
```

```json
{"jsonrpc": "2.0",
 "result": {"r": [[10, 20, 3, -45]], "xd": 0.140172, "yd": 0.088654,
            "x0": 56.8606, "y1": 71.9475, "xc": 640, "yc": 320,
            "t": "20250101T1200", "dt": 3600, "h": [0, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0]},
 "id": 42}
```

### Named parameters, JSON-RPC 1.0 dict

```json
{"method": "get_local_strikes_grid",
 "params": {"x": 101, "y": 202, "data_area": 5, "minute_length": 60},
 "id": 7}
```

```json
{"result": {"r": [], "xd": 0.093761, "yd": 0.089565, "x0": 15.0, "y1": 25.0,
            "xc": 320, "yc": 320, "t": "20250101T1200", "dt": 3600, "h": []},
 "error": null, "id": 7}
```

### Blocked request (invalid user agent)

No `bo-android-*` user agent, so the data request is blocked and returns `{}`
inside the pre-1.0 bare array (id `0`):

```json
{"method": "get_local_strikes_grid", "params": [101, 202, 10000, 60, 0, 0, 5], "id": 0}
```

```json
[{}]
```

### Cluster raster: last hour at 10-minute steps

"Clusters of the last hour at 10-minute steps" is `minute_length = 10`,
`interval_count = 6` (6 intervals x 10 minutes = 60 minutes):

```json
{"jsonrpc": "2.0", "method": "get_global_clusters",
 "params": [10, 0, 6], "id": 8}
```

At `14:32:12` the requested end is `14:32:00`, and the intervals step back by
10 minutes.  When the newest stored cluster is at `14:32` the result's `t` is
`14:32`; when the producer last stored at `14:31` (or `14:30`) the anchor snaps
back and `t` is `14:31` (or `14:30`), with the remaining intervals following
from there:

```json
{"jsonrpc": "2.0",
 "result": {"t": "20250101T1432", "dt": 600,
            "clusters": [{"id": 123, "timestamp": "2025-01-01T14:32:00",
                          "interval_seconds": 600, "strike_count": 17,
                          "area": 3.42, "shape": [[11.0, 51.0], [11.1, 51.0]]}]},
 "id": 8}
```

### Faults

Missing required argument (`FAILURE`, code `8002`):

```json
{"jsonrpc": "2.0", "method": "get_strikes_grid", "id": 1}
```

```json
{"jsonrpc": "2.0",
 "error": {"message": "missing 1 required positional argument: 'minute_length'",
           "code": 8002, "data": ""},
 "id": 1}
```

Unknown method (`METHOD_NOT_FOUND`, code `-32601`):

```json
{"jsonrpc": "2.0", "method": "no_such_method", "params": [], "id": 1}
```

```json
{"jsonrpc": "2.0",
 "error": {"message": "function no_such_method not found", "code": -32601, "data": ""},
 "id": 1}
```