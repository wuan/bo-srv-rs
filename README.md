# bo-service

Rust port of the [blitzortung](https://blitzortung.org) JSON-RPC webservice,
taken from `blitzortung/service/base.py` and friends on `origin/main` of this
repository (async I/O on Tokio instead of Twisted).

The port speaks the same wire protocol as the original service: JSON-RPC over a
raw TCP socket using LSP-style `Content-Length` framing, with the legacy
pre-1.0 dialect for Android clients (`treat_zero_id_as_pre1 = True`).

## Build & test

The checked-in Rust toolchain is broken on this machine; use the stable one:

```sh
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
cargo build --manifest-path rust/bo-service/Cargo.toml
cargo test  --manifest-path rust/bo-service/Cargo.toml
```

The live PostgreSQL integration tests (`tests/postgres_integration.rs`) are
ignored by default; run them against a database with the PostGIS `strikes`
schema:

```sh
export DATABASE_URL="host=127.0.0.1 port=5433 dbname=blitzortung user=blitzortung password=blitzortung"
cargo test --test postgres_integration -- --ignored --nocapture
```

## Run

```sh
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
cargo run --manifest-path rust/bo-service/Cargo.toml

# override the listening port (also: -p)
cargo run --manifest-path rust/bo-service/Cargo.toml -- --port 8300
```

The `service` binary accepts `-p, --port <PORT>` (plus `-h/--help` and
`-V/--version`).  The listening port is resolved with the precedence
**CLI `--port` > `BO_SERVICE_PORT` > `blitzortung.conf` `[webservice] port` >
default `8080`**; the CLI flag wins over both env and file.

At startup a `blitzortung.conf` INI file is searched in `./blitzortung.conf`
then `/etc/blitzortung.conf` (mirroring `blitzortung/config.py`); set
`BO_CONFIG=/path/to/file` to point at an explicit file instead.  The file has
these sections (all `[db]` keys except `connection_count` are required by the
Python `Config`, and `[auth]` is required for the protected data feeds):

> **Note:** the tools read **only** the INI `blitzortung.conf`.  The legacy
> YAML `config.yml`/`config.yaml` (with `blitzortung:` / `database:` keys, see
> `.gitignore`) is **not** read; a missing/absent config file produces a clear
> warning and the built-in defaults are used.

```ini
[webservice]
port = 8300

[db]
host = localhost
port = 5432
dbname = blitzortung
username = blitzortung
password = secret
connection_count = 3

[auth]
username = <blitzortung.org account>
password = <blitzortung.org password>
```

The `[auth]` section holds the HTTP basic-auth credentials for the protected
data feeds (`data.blitzortung.org`), mirroring `Config.get_username()` /
`Config.get_password()`.

Environment variables supplement/override the file (explicit env vars win):

| Variable | Default | Meaning |
| --- | --- | --- |
| `BO_CONFIG` | *(none)* | explicit INI path (bypasses the `.` / `/etc/` search) |
| `BO_SERVICE_PORT` | `8080` | TCP listen port |
| `BO_DB_HOST` | `localhost` | PostgreSQL host |
| `BO_DB_PORT` | `5432` | PostgreSQL port |
| `BO_DB_NAME` | `blitzortung` | database name |
| `BO_DB_USER` | `blitzortung` | database user |
| `BO_DB_PASSWORD` | *(empty)* | database password |
| `BO_DB_CONNECTION_COUNT` | `3` | desired pool size (informational; see below) |
| `BO_BLITZORTUNG_USERNAME` | *(empty)* | `[auth]` username |
| `BO_BLITZORTUNG_PASSWORD` | *(empty)* | `[auth]` password |

The PostgreSQL schema is the normal blitzortung one; the service only reads
`strikes` rows (the `strikes` table with a `geog` geography column, a
`"timestamp"` column, and a `region` column).

## Logging

The service logs one **access line per JSON-RPC request** through the `log`
crate, mirroring the Python `base.py` `log.msg` lines (method + a compact,
size-bounded params summary + client + user agent + handler duration):

```text
INFO  bo_service::transport] get_strikes_grid({"minute_length":60,...}) id=1 client=127.0.0.1 ua=bo-android-190 17.5ms
WARN  bo_service::transport] get_strikes_grid(...) BLOCKED (invalid user agent "Mozilla/5.0") id=2 client=127.0.0.1 ua=Mozilla/5.0 0.1ms
WARN  bo_service::transport] nope([]) fault -32601 "function nope not found" id=3 client=127.0.0.1 ua=bo-android-190 0.0ms
```

* Success and faults are `INFO`; a fault carries the JSON-RPC code/message.
* Rejected data requests log an explicit `BLOCKED` line at `WARN` with the
  reason (blocked IP, invalid user agent, bad content type, referer, or an
  out-of-range grid baseline).
* Request **bodies/credentials are never logged**; only the method, id, and a
  params summary truncated to 160 characters.

The level defaults to `INFO` so these lines are visible out of the box;
override with `RUST_LOG` (e.g. `RUST_LOG=debug`, or `RUST_LOG=warn` to hide the
access lines).

## Protocol

Requests and responses both use LSP-style framing:

```text
Content-Length: <n>\r\n\r\n<json body of exactly n bytes>
```

Additional header lines (`User-Agent`, `Content-Type`, `Referer`,
`X-Forwarded-For`) are parsed into the service request object so the
`base.py` validation rules apply over TCP as well; a peer that sends no such
headers is blocked (invalid user agent).

### Envelope dialects (txjsonrpc_ng semantics)

Applies to every response, mirroring `JSONRPC._select_version` with
`treat_zero_id_as_pre1 = True`:

- explicit `jsonrpc` version field → versioned dict:
  `{"jsonrpc": "2.0", "result": .., "id": ..}`
- no version field and id `0`/missing/falsy → pre-1.0 bare array `[result]`
- no version field and a truthy id → JSON-RPC 1.0 dict
  `{"result": .., "error": null, "id": ..}`

Faults use the matching dialect (`{"faultCode": .., "faultString": ..,
"fault": "Fault"}` pre-1.0, `{"result": null, "error": {...}, "id": ..}` v1,
`{"jsonrpc": "2.0", "error": {...}, "id": ..}` v2).  txjsonrpc always responds
— even to notification-like id-less requests — and non-object bodies (e.g.
batches) are rejected with an invalid-request (-32600) fault in the pre-1.0
dialect, as are missing/unparseable `jsonrpc` version fields.  Faults raised
before `id`/version selection (bad `params` type, missing `method`) always
render as bare pre-1.0 dicts; a missing required argument raises the Python
`TypeError`, which txjsonrpc maps to its generic `FAILURE` (8002) fault code.

### Methods

| Method | Params (defaults) | Result |
| --- | --- | --- |
| `check` | `()` | `{"count": n}` |
| `get_strikes` | `(minute_length, id_or_offset = 0)` | `null` (blocked, log-only) |
| `get_strikes_grid` / `get_strikes_raster` / `get_strokes_raster` | `(minute_length, grid_base_length = 10000, minute_offset = 0, region = 1, count_threshold = 0)` | grid object |
| `get_global_strikes_grid` | `(minute_length, grid_base_length = 10000, minute_offset = 0, count_threshold = 0)` | grid object |
| `get_local_strikes_grid` | `(x, y, grid_base_length = 10000, minute_length = 60, minute_offset = 0, count_threshold = 0, data_area = 5)` | grid object |

Grid result keys (all endpoints): `r, xd, yd, x0, y1, xc, yc, t, dt, h`.
`r` is the strike rows `[rx, ry, count, age]` (region grids flip and filter
the y axis; the global grid uses `ry = -ry - 1`), `xd`/`yd`/`x0`/`y1` are the
rounded grid parameters (`%.6f`/`%.4f`), `t` is `%Y%m%dT%H:%M:%S` of the
interval end, `dt` the interval length in seconds, and `h` the 5-minute
histogram bins (empty when `minute_length <= 10`).

### Validation (base.py order)

- `__to_int` coercion: booleans → 1/0, floats truncate, strings parse,
  anything else (incl. `null`) → blocked request (`{}` result).
- `is_forbidden`: blocked client IP, user agent not `bo-android-<int>`,
  content type ≠ `text/json`, non-empty referer, `grid_base_length` below the
  endpoint minimum (5000 region/local, 25000 global) or not one of the valid
  sizes — checked on the *unclamped* value.
- `grid_base_length = max(minimum, grid_base_length)`.
- `minute_constraints.enforce`: `minute_length` clamped to `[0, 1440]`
  (0 → 60), `minute_offset` clamped to `[-(1440 - minute_length), 0]`.
- `region` clamped to `[1, 7]`, `count_threshold = max(0, ..)`,
  `data_area = max(5, ..)`.
- `fix_bad_accept_header`: `Accept-Encoding` is stripped for
  `bo-android-<version>` with `1 <= version <= 177`.
- Results are cached in a `ServiceCache` (short TTL 20 s, long 60 s, local
  caps 100/400, cleanup 300 s) keyed like the Python producer args.

## Architecture

- `config` — `blitzortung.conf` loading (`.` then `/etc/`) + env overrides;
  make_dsn-compatible connection-string quoting
- `executor` — the `QueryExecutor` trait (`query(sql, params) -> rows`) that
  the service layer depends on
- `mock` — in-memory executor for tests (no PostgreSQL needed)
- `postgres` — production executor over tokio-postgres (single shared client;
  tokio-postgres multiplexes queries over its connection)
- `cache` — `ObjectCache` (TTL + optional LRU size) and the `ServiceCache`
  layout
- `query` — SQL generation matching `blitzortung/db/query.py` /
  `query_builder.py` byte-for-byte in psycopg2 `%(name)s` form and converted
  to `$1..$N` positional parameters
- `service` — the JSON-RPC method handlers with Python-identical validation,
  response shapes, caching and metrics reporting
- `jsonrpc` — JSON-RPC parsing/dispatch and the pre-1.0 / v1 / v2 envelope
  dialects
- `metrics` — the `Metrics` trait (no-op for production, recording impl for
  tests)
- `transport` — `Content-Length` framing, header parsing, and the TCP accept
  loop
- `geom` — the grid/envelope machinery and a UTM converter ported from
  PROJ's Poder/Engsager (`etmerc`) implementation, used by the grid factory
- `round` — CPython-compatible `round()` and `%.Nf` formatting
- `wkb` — WKB encoding of the grid envelope ring / polygons
- `data` — `Timestamp` (nanosecond precision), `Strike` and `GridData`
  (arcgrid/map output), ported from `blitzortung/data.py`
- `builder` — `Strike.from_line` (protected logs) and `Strike.from_json`
- `db` — `StrikeDb` (insert_many/get_latest_time/select/select_strike_keys/
  select_grid) over the `QueryExecutor` trait
- `dataimport` — protected-log URL paths, HTTP/file transports and the strike
  provider
- `websocket` — the Blitzortung live-message `decode()` decompressor
- `util` — `Timer`, `round_time` and `time_intervals`
- `cli` — shared CLI helpers (arg parsing, time zones, file locking) and the
  tool implementations

## CLI tools

Four binaries are ported from the Python `blitzortung/cli` package.  Their
names match bo-python's `pyproject.toml` `[project.scripts]` entries:

| Binary | Python source | Purpose |
| --- | --- | --- |
| `bo-db` | `cli/db.py` | Query strikes as text or a grid (arcgrid/ascii map) |
| `bo-import` | `cli/imprt.py` | Import protected ten-minute strike logs |
| `bo-update` | `cli/update.py` | Import recent strikes from `last_strikes.php` |
| `bo-import-websocket` | `cli/imprt_websocket.py` | Live websocket strike import |

```sh
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"

# last hour, UTC, text output
cargo run --bin bo-db
# explicit interval and area, ECDF-like grid
cargo run --bin bo-db -- --startdate 20250101 --starttime 1200 \
  --enddate 20250101 --endtime 1300 --area "POLYGON((8 45,10 45,10 47,8 47,8 45))" \
  --grid 0.1 --map

# import from data.blitzortung.org (needs [auth] credentials)
cargo run --bin bo-import -- --startdate 20250101
cargo run --bin bo-import -- --update        # now - 30min window
cargo run --bin bo-update -- --hours 2
cargo run --bin bo-import-websocket -- -v
cargo run --bin bo-import-websocket -- -t    # connection test, no DB writes
```

## Documented differences from the Python implementation

- **No statsd daemon.** Metric reporting goes through a `Metrics` trait with
  a no-op production implementation (names match `service/metrics.py`);
  there is no `TimingState` instrumentation.
- **Sequential per-connection handling.** Requests on one connection are
  answered strictly in order; the Twisted service's deferred scheduling is
  not reproduced.  A single tokio-postgres client replaces the connection
  pool (`connection_count` is accepted for compatibility; the multiplexed
  client serves all connections).
- **DB queries are safe inside the Tokio runtime.** The service (multi-thread
  runtime) calls DB queries from async handler tasks; `query`/`execute` use
  `tokio::task::block_in_place` when already inside a runtime, so they no
  longer panic with `Cannot start a runtime from within a runtime`.  Each
  in-flight query still occupies one worker thread (`block_in_place`), so very
  high concurrency (more blocking queries than worker threads) stalls until a
  worker frees up — an async query path would be needed to remove that ceiling.
- **Blocks data requests with no headers.** The Twisted service also blocks
  them (invalid user agent), but over plain TCP the Rust transport reads the
  validation headers from the frame header block, so a bare frame is treated
  as a header-less request.
- **`age` uses total seconds.** The strike age reproduces
  `-int((end_time - timestamp).total_seconds())` without the modulo-86400
  wrapping of `timedelta.seconds`.
- **Python `round()` is round-half-even on the exact binary value**, not the
  simple multiply-by-10 trick; `round::py_round` uses exact decimal
  formatting to match CPython (`round(0.025, 2)` is `0.03`).
- **WKB is 2D** little-endian LineString rings (as produced by
  `shapely.wkb.dumps(LinearRing(...))`); no Z or SRID wrapper.
- **Fixed region table.** The 7-region grid layout is compiled into
  `geom::REGIONS` exactly as defined in `blitzortung/gis/constants.py`.
- **No `utm` crate.** The original port plan used the Rust `utm` crate; its
  accuracy does not match PROJ's etmerc output used by pyproj, so the grid
  factory uses the direct PROJ algorithm port in `geom.rs`, verified to
  `1e-12` against pyproj-generated reference values for all 7 regions.
- **CLI option handling.** The tools use `clap` (derive) instead of `optparse`
  but preserve the Python long/short option names and defaults.  `clap`
  provides `-h`/`--help` and `-V`/`--version` (exit 0) and exits non-zero (2)
  on unknown options or invalid values.
- **Explicit parameter casts for tokio-postgres.** PostgreSQL cannot infer the
  type of some placeholders (e.g. `ST_Transform(geog::geometry, $1)` is
  ambiguous between the `integer` and `text` overloads; `ST_MakePoint($1, $2)`
  between `float8` and `float4`), and defaults them to `text`.  tokio-postgres
  then sends the numeric value in binary form and the server rejects the NUL
  byte with `invalid byte sequence for encoding "UTF8": 0x00` (psycopg2 is
  unaffected because it sends text).  The PostgreSQL rendering therefore adds
  explicit casts (`$1::integer`, `$2::timestamptz`, `$3::smallint`,
  `ST_MakePoint($1::double precision, ...)`); the psycopg2-form SQL from
  `Query::to_sql()` is unchanged and stays byte-for-byte Python-identical.
- **CLI error reporting.** Database errors print the full causal chain
  (`cli::format_error_chain`), including the server-side `severity`/`message`/
  `detail`/`hint` from `tokio_postgres::Error::as_db_error()`, instead of
  tokio-postgres' unhelpful `"db error"` display.  Example:
  `error: db error` / `caused by: ERROR: relation "strikes" does not exist`.
- **Missing configuration is diagnosed.** When no `blitzortung.conf` is found,
  the CLI tools print a prominent warning naming the searched paths and the
  defaults in use (`Config::from_env`), rather than silently using defaults.
  The Python `ConfigModule` raises `No configuration file found` instead; the
  Rust port keeps running with defaults to remain non-breaking.
- **No statsd in the CLI tools.** The Python importers report to a local
  statsd daemon; the Rust tools only log (metrics go through the same
  `Metrics`/plain-logging boundary as the service).
- **Per-region timeout is cooperative.** Python wraps each `bo-import` region
  in `stopit.SignalTimeout(300)`; the Rust port checks the deadline between log
  downloads and uses a 30 second per-request HTTP timeout.
- **Write transactions.** `QueryExecutor::execute` runs each statement in its
  own implicit transaction (tokio-postgres autocommit); `commit()`/`rollback()`
  are accepted for compatibility (`insert_many` is already a single
  multi-value `INSERT`).
- **Streaming vs. buffered downloads.** The Python provider yields strikes
  lazily while streaming each log; the Rust provider collects the lines for a
  region into memory before inserting.
- **`bo-db` grid output.** Python's `db.Strike.select_grid` returns the
  `build_grid_result` tuple, which has no `to_map()`/`to_arcgrid()` (the
  `cli/db.py` grid path only works against a mocked result). The Rust port
  builds a `data.GridData` directly and renders the arcgrid/ascii map as
  `cli/db.py` intends.