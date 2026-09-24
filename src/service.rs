//! Service layer, ported from `blitzortung/service/base.py`,
//! `blitzortung/service/strike_grid.py`, `blitzortung/service/histogram.py`
//! and `blitzortung/service/general.py` on `origin/main`.
//!
//! The `jsonrpc_*` methods mirror the JSON-RPC method handlers; the `get_*`
//! methods are the cached producers (their Python counterparts become the
//! cache key via `creator.name + args + kwargs`).  The cache stores the
//! *result value* only — the envelope (pre-1.0 array / v1 / v2 dict) is
//! re-rendered per request by the JSON-RPC layer.

use std::collections::HashSet;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Map, Value};

use crate::cache::ServiceCache;
use crate::executor::{QueryExecutor, Row};
use crate::geom::{Grid, GridFactory, LocalGrid};
use crate::metrics::Metrics;
use crate::query::{global_grid_query, grid_query, histogram_query, TimeInterval};

pub const SRID: i64 = 4326;
/// `JSON_CONTENT_TYPE` in `base.py`.
pub const JSON_CONTENT_TYPE: &str = "text/json";
/// `USER_AGENT_PREFIX` in `base.py`.
pub const USER_AGENT_PREFIX: &str = "bo-android-";

// Grid validation constants (`Blitzortung` class attributes).
pub const MIN_GRID_BASE_LENGTH: i64 = 5000;
pub const GLOBAL_MIN_GRID_BASE_LENGTH: i64 = 25000;
pub const VALID_GRID_BASE_LENGTHS: [i64; 5] = [5000, 10_000, 25_000, 50_000, 100_000];
pub const MAX_REGION: i64 = 7;

// Time validation constants.
pub const MAX_MINUTES_PER_DAY: i64 = 24 * 60;
pub const DEFAULT_MINUTE_LENGTH: i64 = 60;
pub const HISTOGRAM_MINUTE_THRESHOLD: i64 = 10;

/// User agent validation constant: clients at or below this version get their
/// `Accept-Encoding` header stripped (`fix_bad_accept_header`).
pub const MAX_COMPATIBLE_ANDROID_VERSION: i64 = 177;

/// Histogram bucket size in minutes (`service/histogram.py`).
pub const HISTOGRAM_BIN_SIZE: i64 = 5;

/// Errors that abort a request (the Python counterparts errback the Twisted
/// deferred; the JSON-RPC layer renders them as a fault envelope).
///
/// `Clone` so a cached error can be handed to every waiting request.
#[derive(Debug, Clone)]
pub enum ServiceError {
    /// A database (or mock) query failed.
    Database(String),
    /// A row was missing columns or had an unexpected type.
    MissingColumn(&'static str),
    /// The histogram bin index was out of range (analogue of the Python
    /// `IndexError` that would abort the request).
    HistogramIndex(i64),
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceError::Database(msg) => write!(f, "database error: {msg}"),
            ServiceError::MissingColumn(col) => write!(f, "missing column: {col}"),
            ServiceError::HistogramIndex(idx) => write!(f, "histogram index out of range: {idx}"),
        }
    }
}

impl std::error::Error for ServiceError {}

/// Request metadata the service layer reads — the port of the Twisted
/// `request` object attributes that `base.py` accesses (`getHeader`,
/// `getClientIP`).
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub user_agent: Option<String>,
    pub client_ip: Option<String>,
    pub x_forwarded_for: Option<String>,
    pub content_type: Option<String>,
    pub referer: Option<String>,
    /// Raw `Accept-Encoding` request header (the txjsonrpc renderer reads it
    /// per response via `getHeader('Accept-encoding')`).
    pub accept_encoding: Option<String>,
    /// `fix_bad_accept_header` stripped the `Accept-Encoding` header because
    /// the client is at or below `MAX_COMPATIBLE_ANDROID_VERSION`.
    pub accept_encoding_removed: bool,
    /// Set by the data handlers when `is_forbidden` rejects the request; the
    /// reason is surfaced in the access log (`BLOCKED`).  `None` for requests
    /// that were not blocked.
    pub blocked_reason: Option<String>,
}

impl Request {
    /// `base.Blitzortung.get_request_client`: the first X-Forwarded-For
    /// component, or the client IP when no X-Forwarded-For header is present.
    /// An empty header value is falsy in Python and falls through.
    pub fn request_client(&self) -> Option<String> {
        match &self.x_forwarded_for {
            Some(forward) if !forward.is_empty() => {
                Some(forward.split(',').next().unwrap_or("").trim().to_string())
            }
            _ => self.client_ip.clone(),
        }
    }

    /// `base.parse_user_agent`: the integer version of a `bo-android-<int>`
    /// user agent, `0` when the header is missing or does not match.
    pub fn user_agent_version(&self) -> i64 {
        let Some(user_agent) = self.user_agent.as_deref() else {
            return 0;
        };
        match user_agent.split(' ').next().and_then(|word| word.rsplit_once('-')) {
            Some(("bo-android", version)) => version.parse::<i64>().unwrap_or(0),
            _ => 0,
        }
    }

    /// `base.fix_bad_accept_header`: old Android clients (< version 178) have
    /// gzip bugs; their `Accept-Encoding` header is removed.  Returns whether
    /// the header was stripped.
    pub fn fix_bad_accept_header(&mut self) -> bool {
        let version = self.user_agent_version();
        if version > 0 && version <= MAX_COMPATIBLE_ANDROID_VERSION {
            self.accept_encoding_removed = true;
            true
        } else {
            false
        }
    }

    /// Whether the response may be gzip-compressed for this request.
    ///
    /// Mirrors `txjsonrpc_ng.web.render.Renderer.handle_compression`: the
    /// `Accept-Encoding` header must list `gzip` (case-insensitively, comma
    /// separated) and `fix_bad_accept_header` must not have stripped it for an
    /// old Android client.  The size threshold (>= 1000 bytes) is applied by
    /// the caller where the rendered body length is known.
    pub fn accepts_gzip(&self) -> bool {
        if self.accept_encoding_removed {
            return false;
        }
        self.accept_encoding
            .as_deref()
            .is_some_and(|value| {
                value
                    .split(',')
                    .any(|encoding| encoding.trim().eq_ignore_ascii_case("gzip"))
            })
    }
}

/// `blitzortung.util.TimeConstraint`.
struct TimeConstraint {
    default_minute_length: i64,
    max_minute_length: i64,
}

impl TimeConstraint {
    fn new(default_minute_length: i64, max_minute_length: i64) -> Self {
        TimeConstraint {
            default_minute_length,
            max_minute_length,
        }
    }

    /// `TimeConstraint.enforce(minute_length, minute_offset)`.
    fn enforce(&self, minute_length: i64, minute_offset: i64) -> (i64, i64) {
        let minute_length = force_range(0, minute_length, self.max_minute_length);
        let minute_length = if minute_length == 0 {
            self.default_minute_length
        } else {
            minute_length
        };
        let minute_offset = force_range(-self.max_minute_length + minute_length, minute_offset, 0);
        (minute_length, minute_offset)
    }
}

/// The JSON-RPC service (`blitzortung.service.base.Blitzortung`).
pub struct Service<M: Metrics = crate::metrics::NoopMetrics> {
    executor: Arc<dyn QueryExecutor>,
    cache: ServiceCache,
    metrics: M,
    forbidden_ips: HashSet<String>,
    /// `check_count`: a monotonically increasing counter for `jsonrpc_check`.
    check_count: AtomicI64,
    minute_constraints: TimeConstraint,
}

impl<M: Metrics> Service<M> {
    pub fn with_parts(
        executor: Arc<dyn QueryExecutor>,
        cache: ServiceCache,
        metrics: M,
        forbidden_ips: HashSet<String>,
    ) -> Self {
        Service {
            executor,
            cache,
            metrics,
            forbidden_ips,
            check_count: AtomicI64::new(0),
            minute_constraints: TimeConstraint::new(DEFAULT_MINUTE_LENGTH, MAX_MINUTES_PER_DAY),
        }
    }

    pub fn executor(&self) -> &dyn QueryExecutor {
        self.executor.as_ref()
    }

    pub fn cache(&self) -> &ServiceCache {
        &self.cache
    }

    pub fn forbidden_ips(&self) -> &HashSet<String> {
        &self.forbidden_ips
    }

    // -----------------------------------------------------------------------
    // handlers
    // -----------------------------------------------------------------------

    /// `jsonrpc_check` -> `{"count": n}`.
    pub fn check(&self) -> Value {
        let count = self.check_count.fetch_add(1, Ordering::SeqCst) + 1;
        json!({"count": count})
    }

    /// `jsonrpc_get_strikes` — currently blocked for all requests.
    ///
    /// Returns `None` when the parameters fail the `__to_int` coercion, which
    /// the JSON-RPC layer renders as a `null` result.
    pub fn get_strikes(&self, request: &Request, minute_length: &Value, id_or_offset: &Value) -> Option<Value> {
        let minute_length = to_int(minute_length)?;
        let _id_or_offset = to_int(id_or_offset)?;
        let _minute_length = force_range(0, minute_length, MAX_MINUTES_PER_DAY);
        let _client = request.request_client();
        let _user_agent = request.user_agent.clone();
        None
    }

    /// `get_strikes_grid` producer (cached inside `strikes_grid`).
    async fn get_strikes_grid(
        &self,
        minute_length: i64,
        grid_baselength: i64,
        minute_offset: i64,
        region: i64,
        count_threshold: i64,
    ) -> Result<Value, ServiceError> {
        let grid = match GridFactory::for_region(region as u32) {
            Some(factory) => factory.get_for(grid_baselength as f64),
            None => GridFactory::global().get_for(grid_baselength as f64),
        };
        let time_interval = create_time_interval(minute_length, minute_offset);
        let grid_data = self.run_grid_query(&grid, &time_interval, Some(region), count_threshold, false).await?;
        let histogram = if minute_length > HISTOGRAM_MINUTE_THRESHOLD {
            self.get_histogram(
                &time_interval,
                None,
                Some(&grid),
                &histogram_cache_key(minute_length, minute_offset, Some(&grid)),
            )
            .await?
        } else {
            vec![]
        };
        Ok(Self::build_grid_response(grid_data, histogram, &grid, &time_interval))
    }

    /// `get_global_strikes_grid` producer (cached inside `global_strikes_grid`).
    async fn get_global_strikes_grid(
        &self,
        minute_length: i64,
        grid_baselength: i64,
        minute_offset: i64,
        count_threshold: i64,
    ) -> Result<Value, ServiceError> {
        let grid = GridFactory::global().get_for(grid_baselength as f64);
        let time_interval = create_time_interval(minute_length, minute_offset);
        let grid_data = self.run_grid_query(&grid, &time_interval, None, count_threshold, true).await?;
        let histogram = if minute_length > HISTOGRAM_MINUTE_THRESHOLD {
            self.get_histogram(
                &time_interval,
                None,
                None,
                &histogram_cache_key(minute_length, minute_offset, None),
            )
            .await?
        } else {
            vec![]
        };
        Ok(Self::build_grid_response(grid_data, histogram, &grid, &time_interval))
    }

    /// `get_local_strikes_grid` producer (cached inside `local_strikes_grid`).
    #[allow(clippy::too_many_arguments)]
    async fn get_local_strikes_grid(
        &self,
        x: i64,
        y: i64,
        grid_baselength: i64,
        minute_length: i64,
        minute_offset: i64,
        count_threshold: i64,
        data_area: i64,
    ) -> Result<Value, ServiceError> {
        let local_grid = LocalGrid {
            data_area,
            x,
            y,
        };
        let grid = local_grid.grid_factory().get_for(grid_baselength as f64);
        let time_interval = create_time_interval(minute_length, minute_offset);
        let grid_data = self.run_grid_query(&grid, &time_interval, None, count_threshold, false).await?;
        let histogram = if minute_length > HISTOGRAM_MINUTE_THRESHOLD {
            self.get_histogram(
                &time_interval,
                None,
                Some(&grid),
                &histogram_cache_key(minute_length, minute_offset, Some(&grid)),
            )
            .await?
        } else {
            vec![]
        };
        Ok(Self::build_grid_response(grid_data, histogram, &grid, &time_interval))
    }

    /// Run the grid SQL and shape the rows (`StrikeGridQuery.create` /
    /// `GlobalStrikeGridQuery.create` plus `db.grid_result.build_grid_result`).
    async fn run_grid_query(
        &self,
        grid: &Grid,
        time_interval: &TimeInterval,
        region: Option<i64>,
        count_threshold: i64,
        global: bool,
    ) -> Result<Vec<Value>, ServiceError> {
        let query = if global {
            global_grid_query(grid, time_interval, count_threshold)
        } else {
            grid_query(grid, time_interval, region, count_threshold)
        };
        let rows = self
            .executor
            .query(&query.to_postgres(), &query.parameters())
            .await
            .map_err(|e| ServiceError::Database(e.to_string()))?;
        Ok(build_grid_rows(&rows, grid, time_interval.end, global))
    }

    /// `base.get_histogram`: run (or fetch from the histogram cache) the
    /// histogram bins for `time_interval`.
    async fn get_histogram(
        &self,
        time_interval: &TimeInterval,
        region: Option<i64>,
        envelope: Option<&Grid>,
        cache_key: &str,
    ) -> Result<Vec<Value>, ServiceError> {
        let key = format!("histogram_query|{cache_key}");
        // The producer is async and may be shared with concurrent callers; the
        // cache stores the in-flight computation (single-flight).
        let result = self
            .cache
            .histogram
            .get_result(&key, || async {
                let query = histogram_query(time_interval, HISTOGRAM_BIN_SIZE, region, envelope);
                let rows = self
                    .executor
                    .query(&query.to_postgres(), &query.parameters())
                    .await
                    .map_err(|e| cache_error(ServiceError::Database(e.to_string())))?;
                build_histogram(&rows, time_interval.minutes(), HISTOGRAM_BIN_SIZE)
                    .map(Value::Array)
                    .map_err(cache_error)
            })
            .await;
        self.metrics
            .for_histogram(self.cache.histogram.get_ratio(), self.cache.histogram.get_size());
        match result {
            Ok(Value::Array(bins)) => Ok(bins),
            // The histogram cache only ever stores arrays.
            Ok(_) => Ok(vec![]),
            Err(error) => Err(service_error(error)),
        }
    }

    /// `StrikeGridQuery.build_grid_response` / `GlobalStrikeGridQuery.build_grid_response`
    /// (both produce the same ten keys, including `x0`/`y1`/`xc`/`yc`).
    fn build_grid_response(
        grid_data: Vec<Value>,
        histogram_data: Vec<Value>,
        grid: &Grid,
        time_interval: &TimeInterval,
    ) -> Value {
        let mut response = Map::new();
        response.insert("r".into(), json!(grid_data));
        response.insert("xd".into(), json!(crate::round::py_round(grid.x_div, 6)));
        response.insert("yd".into(), json!(crate::round::py_round(grid.y_div, 6)));
        response.insert("x0".into(), json!(crate::round::py_round(grid.x_min, 4)));
        response.insert("y1".into(), json!(crate::round::py_round(grid.y_max + grid.y_div, 4)));
        response.insert("xc".into(), json!(grid.x_bin_count()));
        response.insert("yc".into(), json!(grid.y_bin_count()));
        response.insert("t".into(), json!(strftime_yyyymmddthms(time_interval.end)));
        response.insert("dt".into(), json!(time_interval.duration_seconds()));
        response.insert("h".into(), json!(histogram_data));
        Value::Object(response)
    }

    /// `base.is_forbidden`: a data request is rejected when the client IP is
    /// blocked, the user agent is not a valid `bo-android-<int>` client, the
    /// content type is not `text/json`, a referer is set, or the grid baseline
    /// is below the endpoint minimum or not one of the supported sizes.
    ///
    /// Returns the human-readable reason when forbidden (`None` when allowed),
    /// matching the `FORBIDDEN - client: .., user agent: ..` diagnostic of
    /// `base.py` and driving the access log's `BLOCKED` line.
    fn forbidden_reason(
        &self,
        request: &Request,
        client: Option<&str>,
        user_agent_version: i64,
        grid_base_length: i64,
        min_grid_base_length: i64,
    ) -> Option<String> {
        let content_type = request.content_type.as_deref();
        let referer = request.referer.as_deref();
        if client.is_some_and(|client| self.forbidden_ips.contains(client)) {
            Some(format!("blocked ip {client:?}"))
        } else if user_agent_version == 0 {
            Some(format!(
                "invalid user agent {:?}",
                request.user_agent.as_deref().unwrap_or("")
            ))
        } else if content_type != Some(JSON_CONTENT_TYPE) {
            Some(format!("bad content type {content_type:?}"))
        } else if referer.is_some_and(|referer| !referer.is_empty()) {
            Some(format!("referer {referer:?}"))
        } else if grid_base_length < min_grid_base_length {
            Some(format!(
                "grid_base_length {grid_base_length} below minimum {min_grid_base_length}"
            ))
        } else if !VALID_GRID_BASE_LENGTHS.contains(&grid_base_length) {
            Some(format!("invalid grid_base_length {grid_base_length}"))
        } else {
            None
        }
    }
}

impl Service<crate::metrics::NoopMetrics> {
    /// A ready-to-use service with a no-op metrics implementation.
    pub fn new(executor: Arc<dyn QueryExecutor>) -> Self {
        Service::with_parts(executor, ServiceCache::new(), crate::metrics::NoopMetrics, HashSet::new())
    }
}

// ---------------------------------------------------------------------------
// jsonrpc_* method pipelines (validation + cache + metrics, mirroring base.py)
// ---------------------------------------------------------------------------

impl<M: Metrics> Service<M> {
    /// `jsonrpc_get_strikes_grid` (also the target of the ``*_raster`` aliases).
    #[allow(clippy::too_many_arguments)]
    pub async fn jsonrpc_get_strikes_grid(
        &self,
        request: &mut Request,
        minute_length: &Value,
        grid_base_length: &Value,
        minute_offset: &Value,
        region: &Value,
        count_threshold: &Value,
    ) -> Value {
        let (Some(minute_length), Some(grid_base_length), Some(minute_offset), Some(region), Some(count_threshold)) =
            (to_int(minute_length), to_int(grid_base_length), to_int(minute_offset), to_int(region), to_int(count_threshold))
        else {
            return json!({});
        };

        let client = request.request_client();
        let user_agent_version = request.user_agent_version();

        if let Some(reason) = self.forbidden_reason(
            request,
            client.as_deref(),
            user_agent_version,
            grid_base_length,
            MIN_GRID_BASE_LENGTH,
        ) {
            request.blocked_reason = Some(reason);
            return json!({});
        }

        let original_grid_base_length = grid_base_length;
        let grid_base_length = i64::max(MIN_GRID_BASE_LENGTH, grid_base_length);
        let (minute_length, minute_offset) = self.minute_constraints.enforce(minute_length, minute_offset);
        let region = force_range(1, region, MAX_REGION);
        let count_threshold = i64::max(0, count_threshold);

        let cache_key = format!(
            "get_strikes_grid|minute_length={minute_length}|grid_baselength={grid_base_length}|\
             minute_offset={minute_offset}|region={region}|count_threshold={count_threshold}"
        );
        let response = self
            .cache
            .strikes(minute_offset)
            .get_result(&cache_key, || async {
                self.get_strikes_grid(minute_length, grid_base_length, minute_offset, region, count_threshold)
                    .await
                    .map_err(cache_error)
            })
            .await
            .map_err(service_error)
            .unwrap_or(Value::Null);
        let _ = request.fix_bad_accept_header();

        let _ = (original_grid_base_length, minute_offset);
        self.metrics.for_strikes(minute_length, region, self.cache.strikes(minute_offset).get_ratio());
        response
    }

    /// `jsonrpc_get_strikes_raster` / `jsonrpc_get_strokes_raster`.
    pub async fn jsonrpc_get_strikes_raster(
        &self,
        request: &mut Request,
        minute_length: &Value,
        grid_base_length: &Value,
        minute_offset: &Value,
        region: &Value,
    ) -> Value {
        self.jsonrpc_get_strikes_grid(
            request,
            minute_length,
            grid_base_length,
            minute_offset,
            region,
            &json!(0),
        )
        .await
    }

    /// `jsonrpc_get_global_strikes_grid`.
    pub async fn jsonrpc_get_global_strikes_grid(
        &self,
        request: &mut Request,
        minute_length: &Value,
        grid_base_length: &Value,
        minute_offset: &Value,
        count_threshold: &Value,
    ) -> Value {
        let (Some(minute_length), Some(grid_base_length), Some(minute_offset), Some(count_threshold)) =
            (to_int(minute_length), to_int(grid_base_length), to_int(minute_offset), to_int(count_threshold))
        else {
            return json!({});
        };

let client = request.request_client();
        let user_agent_version = request.user_agent_version();

        if let Some(reason) = self.forbidden_reason(
            request,
            client.as_deref(),
            user_agent_version,
            grid_base_length,
            GLOBAL_MIN_GRID_BASE_LENGTH,
        ) {
            request.blocked_reason = Some(reason);
            return json!({});
        }

        let original_grid_base_length = grid_base_length;
        let grid_base_length = i64::max(GLOBAL_MIN_GRID_BASE_LENGTH, grid_base_length);
        let (minute_length, minute_offset) = self.minute_constraints.enforce(minute_length, minute_offset);
        let count_threshold = i64::max(0, count_threshold);

        let cache_key = format!(
            "get_global_strikes_grid|minute_length={minute_length}|grid_baselength={grid_base_length}|\
             minute_offset={minute_offset}|count_threshold={count_threshold}"
        );
        let response = self
            .cache
            .global_strikes(minute_offset)
            .get_result(&cache_key, || async {
                self.get_global_strikes_grid(minute_length, grid_base_length, minute_offset, count_threshold)
                    .await
                    .map_err(cache_error)
            })
            .await
            .map_err(service_error)
            .unwrap_or(Value::Null);
        let _ = request.fix_bad_accept_header();

        let _ = (original_grid_base_length, minute_offset);
        self.metrics
            .for_global_strikes(minute_length, self.cache.global_strikes(minute_offset).get_ratio());
        response
    }

    /// `jsonrpc_get_local_strikes_grid`.
    #[allow(clippy::too_many_arguments)]
    pub async fn jsonrpc_get_local_strikes_grid(
        &self,
        request: &mut Request,
        x: &Value,
        y: &Value,
        grid_base_length: &Value,
        minute_length: &Value,
        minute_offset: &Value,
        count_threshold: &Value,
        data_area: &Value,
    ) -> Value {
        let (
            Some(x),
            Some(y),
            Some(grid_base_length),
            Some(minute_length),
            Some(minute_offset),
            Some(count_threshold),
            Some(data_area),
        ) = (
            to_int(x),
            to_int(y),
            to_int(grid_base_length),
            to_int(minute_length),
            to_int(minute_offset),
            to_int(count_threshold),
            to_int(data_area),
        )
        else {
            return json!({});
        };

        let client = request.request_client();
        let user_agent_version = request.user_agent_version();

        if let Some(reason) = self.forbidden_reason(
            request,
            client.as_deref(),
            user_agent_version,
            grid_base_length,
            MIN_GRID_BASE_LENGTH,
        ) {
            request.blocked_reason = Some(reason);
            return json!({});
        }

        let original_grid_base_length = grid_base_length;
        let grid_base_length = i64::max(MIN_GRID_BASE_LENGTH, grid_base_length);
        let (minute_length, minute_offset) = self.minute_constraints.enforce(minute_length, minute_offset);
        let data_area = round_max_5(data_area);
        let count_threshold = i64::max(0, count_threshold);

        let cache_key = format!(
            "get_local_strikes_grid|x={x}|y={y}|grid_baselength={grid_base_length}|minute_length={minute_length}|\
             minute_offset={minute_offset}|count_threshold={count_threshold}|data_area={data_area}"
        );
        let response = self
            .cache
            .local_strikes(minute_offset)
            .get_result(&cache_key, || async {
                self.get_local_strikes_grid(
                    x, y, grid_base_length, minute_length, minute_offset, count_threshold, data_area,
                )
                .await
                .map_err(cache_error)
            })
            .await
            .map_err(service_error)
            .unwrap_or(Value::Null);

        let _ = (original_grid_base_length, minute_offset);
        self.metrics.for_local_strikes(
            minute_length,
            data_area,
            self.cache.local_strikes(minute_offset).get_ratio(),
        );
        response
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Erase a [`ServiceError`] into the cache's shared error type.
fn cache_error(error: ServiceError) -> crate::cache::CacheError {
    std::sync::Arc::new(error)
}

/// Recover a [`ServiceError`] from a cached (type-erased) error.
///
/// The cache only ever stores `ServiceError`s produced by this module, so the
/// downcast succeeds; a non-`ServiceError` (impossible in practice) is wrapped
/// as a database error so no error is silently swallowed.
fn service_error(error: crate::cache::CacheError) -> ServiceError {
    match error.downcast_ref::<ServiceError>() {
        Some(service_error) => service_error.clone(),
        None => ServiceError::Database(error.to_string()),
    }
}

/// `blitzortung.util.force_range(lower_limit, value, upper_limit)`.
fn force_range(lower_limit: i64, value: i64, upper_limit: i64) -> i64 {
    if value < lower_limit {
        lower_limit
    } else if value > upper_limit {
        upper_limit
    } else {
        value
    }
}

/// Python `round(max(5, data_area))` for the local grid data area: the value
/// is an integer so `round` is the identity; only the `max(5, ..)` clamping
/// is observable.
fn round_max_5(data_area: i64) -> i64 {
    i64::max(5, data_area)
}

/// `base.Blitzortung.__to_int`: Python `int()` semantics — booleans coerce to
/// 1/0, floats truncate toward zero, strings parse as integers; anything else
/// (including `None`) yields `None`.
fn to_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|f| f.trunc() as i64)),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        Value::Bool(true) => Some(1),
        Value::Bool(false) => Some(0),
        _ => None,
    }
}

/// Histogram cache key (`(minute_length, minute_offset[, grid])` as a string).
fn histogram_cache_key(minute_length: i64, minute_offset: i64, grid: Option<&Grid>) -> String {
    let mut key = format!("minute_length={minute_length}|minute_offset={minute_offset}");
    if let Some(grid) = grid {
        // The Python key compares the Grid object; a stable string built from
        // its six parameters is equivalent (the same grid always renders the
        // same signature).
        key.push_str(&format!(
            "|grid={:?},{:?},{:?},{:?},{:?},{:?}",
            grid.x_min, grid.x_max, grid.y_min, grid.y_max, grid.x_div, grid.y_div
        ));
    }
    key
}

/// `blitzortung.service.general.create_time_interval`:
/// end = now (UTC, truncated to seconds) + minute_offset minutes;
/// start = end - minute_length minutes.
pub fn create_time_interval(minute_length: i64, minute_offset: i64) -> TimeInterval {
    create_time_interval_at(minute_length, minute_offset, Utc::now())
}

/// Testable variant of [`create_time_interval`] with an explicit clock.
pub fn create_time_interval_at(minute_length: i64, minute_offset: i64, now: DateTime<Utc>) -> TimeInterval {
    let now_secs = now.timestamp();
    let end = DateTime::<Utc>::from_timestamp(now_secs, 0)
        .expect("valid timestamp")
        + Duration::minutes(minute_offset);
    let start = end - Duration::minutes(minute_length);
    TimeInterval::new(start, end)
}

/// Format `%Y%m%dT%H:%M:%S` (UTC).
fn strftime_yyyymmddthms(ts: DateTime<Utc>) -> String {
    ts.format("%Y%m%dT%H:%M:%S").to_string()
}

/// `histogram.HistogramQuery.build_result`: bin the per-interval counts.
///
/// `value_count = int(minutes / bin_size)`; index = `interval +
/// value_count - 1` with Python's negative-index wrap; out-of-range indexes
/// abort the request (Python `IndexError`).
pub fn build_histogram(rows: &[Row], minutes: i64, bin_size: i64) -> Result<Vec<Value>, ServiceError> {
    let value_count = (minutes / bin_size) as usize;
    let mut result: Vec<Value> = vec![json!(0); value_count];

    for row in rows {
        let interval = row
            .get_i64(0)
            .ok_or(ServiceError::MissingColumn("interval"))?;
        let count = row
            .get_i64(1)
            .ok_or(ServiceError::MissingColumn("count"))?;
        let mut index = interval + value_count as i64 - 1;
        if index < 0 {
            index = index.rem_euclid(value_count as i64);
        }
        if index < 0 || index >= value_count as i64 {
            return Err(ServiceError::HistogramIndex(interval));
        }
        result[index as usize] = json!(count);
    }

    Ok(result)
}

/// Shape the grid rows (`db.grid_result.build_grid_result` for region grids,
/// `GlobalStrikeGridQuery.build_result` for global grids).
///
/// The strike age is `-int((end_time - timestamp).total_seconds())` with the
/// *total* seconds (no modulo-86400 wrapping), truncated toward zero.
fn build_grid_rows(rows: &[Row], grid: &Grid, end_time: DateTime<Utc>, global: bool) -> Vec<Value> {
    let x_bin_count = grid.x_bin_count();
    let y_bin_count = grid.y_bin_count();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let rx = row.get_i64(0).unwrap_or(0);
        let ry = row.get_i64(1).unwrap_or(0);
        let count = row.get_i64(2).unwrap_or(0);
        let timestamp = row.get_timestamp(3).unwrap_or(end_time);
        let age = -((end_time - timestamp).num_seconds());
        if global {
            out.push(json!([rx, -ry - 1, count, age]));
        } else if (0..x_bin_count).contains(&rx) && (1..=y_bin_count).contains(&ry) {
            out.push(json!([rx, y_bin_count - ry, count, age]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::Value as DBValue;
    use crate::mock::MockExecutor;

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(secs, 0).unwrap()
    }

    fn fixed_now() -> DateTime<Utc> {
        ts(1_700_000_000)
    }

    fn req_with(client: &str) -> Request {
        Request {
            user_agent: Some("bo-android-190".to_string()),
            client_ip: Some(client.to_string()),
            content_type: Some(JSON_CONTENT_TYPE.to_string()),
            ..Default::default()
        }
    }

    /// Test shim for the removed `is_forbidden` (now `forbidden_reason`).
    fn is_forbidden(
        service: &Service,
        request: &Request,
        client: Option<&str>,
        user_agent_version: i64,
        grid_base_length: i64,
        min_grid_base_length: i64,
    ) -> bool {
        service
            .forbidden_reason(
                request,
                client,
                user_agent_version,
                grid_base_length,
                min_grid_base_length,
            )
            .is_some()
    }

    #[test]
    fn check_counts_monotonically() {
        let service = Service::new(Arc::new(MockExecutor::new()));
        assert_eq!(service.check(), json!({"count": 1}));
        assert_eq!(service.check(), json!({"count": 2}));
        assert_eq!(service.check(), json!({"count": 3}));
    }

    #[test]
    fn get_strikes_is_blocked() {
        let service = Service::new(Arc::new(MockExecutor::new()));
        assert_eq!(
            service.get_strikes(&req_with("1.2.3.4"), &json!(60), &json!(0)),
            None
        );
        // invalid arguments also produce None
        assert_eq!(
            service.get_strikes(&req_with("1.2.3.4"), &json!("x"), &json!(0)),
            None
        );
    }

    #[test]
    fn user_agent_version_parsing() {
        let mut base = Request {
            user_agent: None,
            client_ip: Some("1.2.3.4".to_string()),
            content_type: Some(JSON_CONTENT_TYPE.to_string()),
            ..Default::default()
        };
        assert_eq!(base.user_agent_version(), 0);
        for (ua, version) in [
            ("bo-android-190", 190),
            ("bo-android-1", 1),
            ("bo-android-178", 178),
            ("bo-android-0", 0),
            ("bo-android--5", 0),
            ("bo-android-abc", 0),
            ("bo-android", 0),
            ("bo-android-", 0),
            ("bo_android_150", 0),
            ("BO-ANDROID-150", 0),
            ("python-requests/2.32", 0),
            ("Mozilla/5.0 (Linux; Android 13)", 0),
        ] {
            base.user_agent = Some(ua.to_string());
            assert_eq!(base.user_agent_version(), version, "user agent {ua:?}");
        }
    }

    #[test]
    fn request_client_uses_forwarded_for_first() {
        let req = Request {
            x_forwarded_for: Some(" 203.0.113.7, 10.0.0.1 ".to_string()),
            client_ip: Some("10.0.0.9".to_string()),
            ..Default::default()
        };
        assert_eq!(req.request_client(), Some("203.0.113.7".to_string()));
        let req = Request {
            x_forwarded_for: Some(String::new()),
            client_ip: Some("10.0.0.9".to_string()),
            ..Default::default()
        };
        assert_eq!(req.request_client(), Some("10.0.0.9".to_string()));
    }

    #[test]
    fn fix_bad_accept_header_only_for_old_clients() {
        let mut req = Request {
            user_agent: Some("bo-android-177".to_string()),
            ..Default::default()
        };
        assert!(req.fix_bad_accept_header());
        assert!(req.accept_encoding_removed);

        let mut req = Request {
            user_agent: Some("bo-android-178".to_string()),
            ..Default::default()
        };
        assert!(!req.fix_bad_accept_header());

        let mut req = Request {
            user_agent: Some("bo-android-abc".to_string()),
            ..Default::default()
        };
        assert!(!req.fix_bad_accept_header());
    }

    /// The compression policy: `gzip` must be advertised, and an old Android
    /// client's stripped header must win over the raw value
    /// (`Renderer.handle_compression` + `fix_bad_accept_header`).
    #[test]
    fn accepts_gzip_matches_accept_encoding_and_client_version() {
        let gzip = Request {
            accept_encoding: Some("gzip".to_string()),
            ..Default::default()
        };
        assert!(gzip.accepts_gzip());

        let gzip_mixed = Request {
            accept_encoding: Some("deflate, GZIP".to_string()),
            ..Default::default()
        };
        assert!(gzip_mixed.accepts_gzip());

        // `txjsonrpc_ng` only splits on commas (no q-value parsing), so a
        // quality parameter does not match a bare `gzip` token.
        let gzip_with_qvalue = Request {
            accept_encoding: Some("gzip;q=0.5".to_string()),
            ..Default::default()
        };
        assert!(!gzip_with_qvalue.accepts_gzip());

        let identity = Request {
            accept_encoding: Some("identity".to_string()),
            ..Default::default()
        };
        assert!(!identity.accepts_gzip());

        let missing = Request::default();
        assert!(!missing.accepts_gzip());

        // An old client is downgraded: even with `Accept-Encoding: gzip` the
        // header is treated as removed after `fix_bad_accept_header`.
        let mut old_client = Request {
            user_agent: Some("bo-android-177".to_string()),
            accept_encoding: Some("gzip".to_string()),
            ..Default::default()
        };
        assert!(old_client.fix_bad_accept_header());
        assert!(!old_client.accepts_gzip());

        // A new client keeps it.
        let new_client = Request {
            user_agent: Some("bo-android-178".to_string()),
            accept_encoding: Some("gzip".to_string()),
            ..Default::default()
        };
        assert!(new_client.accepts_gzip());
    }

    #[test]
    fn forbidden_rules_are_checked() {
        let mut forbidden = HashSet::new();
        forbidden.insert("1.2.3.4".to_string());
        let service = Service::with_parts(
            Arc::new(MockExecutor::new()),
            ServiceCache::new(),
            crate::metrics::NoopMetrics,
            forbidden,
        );
        let blocked_ip = Request {
            user_agent: Some("bo-android-190".to_string()),
            client_ip: Some("1.2.3.4".to_string()),
            content_type: Some(JSON_CONTENT_TYPE.to_string()),
            ..Default::default()
        };
        assert!(is_forbidden(&service, &blocked_ip, Some("1.2.3.4"), 190, 10_000, MIN_GRID_BASE_LENGTH));

        let bad_ua = Request {
            user_agent: Some("Mozilla/5.0".to_string()),
            client_ip: Some("5.6.7.8".to_string()),
            content_type: Some(JSON_CONTENT_TYPE.to_string()),
            ..Default::default()
        };
        assert!(is_forbidden(&service, &bad_ua, Some("5.6.7.8"), 0, 10_000, MIN_GRID_BASE_LENGTH));

        let bad_content_type = Request {
            user_agent: Some("bo-android-190".to_string()),
            client_ip: Some("5.6.7.8".to_string()),
            content_type: Some("application/json".to_string()),
            ..Default::default()
        };
        assert!(is_forbidden(&service, 
            &bad_content_type,
            Some("5.6.7.8"),
            190,
            10_000,
            MIN_GRID_BASE_LENGTH
        ));

        let referer = Request {
            user_agent: Some("bo-android-190".to_string()),
            client_ip: Some("5.6.7.8".to_string()),
            content_type: Some(JSON_CONTENT_TYPE.to_string()),
            referer: Some("http://example.com".to_string()),
            ..Default::default()
        };
        assert!(is_forbidden(&service, &referer, Some("5.6.7.8"), 190, 10_000, MIN_GRID_BASE_LENGTH));

        let low_baseline = Request {
            user_agent: Some("bo-android-190".to_string()),
            client_ip: Some("5.6.7.8".to_string()),
            content_type: Some(JSON_CONTENT_TYPE.to_string()),
            ..Default::default()
        };
        assert!(is_forbidden(&service, 
            &low_baseline,
            Some("5.6.7.8"),
            190,
            1000,
            MIN_GRID_BASE_LENGTH
        ));
        // unsupported size in the valid set
        assert!(is_forbidden(&service, 
            &low_baseline,
            Some("5.6.7.8"),
            190,
            20_000,
            MIN_GRID_BASE_LENGTH
        ));
        // a valid request is not forbidden
        let valid = Request {
            user_agent: Some("bo-android-190".to_string()),
            client_ip: Some("5.6.7.8".to_string()),
            content_type: Some(JSON_CONTENT_TYPE.to_string()),
            ..Default::default()
        };
        assert!(!is_forbidden(&service, &valid, Some("5.6.7.8"), 190, 10_000, MIN_GRID_BASE_LENGTH));
    }

    #[test]
    fn to_int_coercion_matches_python() {
        assert_eq!(to_int(&json!(190)), Some(190));
        assert_eq!(to_int(&json!(1.7)), Some(1));
        assert_eq!(to_int(&json!(-1.7)), Some(-1));
        assert_eq!(to_int(&json!("12")), Some(12));
        assert_eq!(to_int(&json!(" 12 ")), Some(12));
        assert_eq!(to_int(&json!("12.5")), None);
        assert_eq!(to_int(&json!(true)), Some(1));
        assert_eq!(to_int(&json!(false)), Some(0));
        assert_eq!(to_int(&Value::Null), None);
        assert_eq!(to_int(&json!([1])), None);
    }

    #[test]
    fn minute_constraints_enforce() {
        let constraints = TimeConstraint::new(DEFAULT_MINUTE_LENGTH, MAX_MINUTES_PER_DAY);
        // 0 -> default, negative offsets clamped to -1440+ml
        assert_eq!(constraints.enforce(0, 0), (60, 0));
        assert_eq!(constraints.enforce(30, -10000), (30, -1410));
        assert_eq!(constraints.enforce(2000, 500), (1440, 0));
        assert_eq!(constraints.enforce(60, -5), (60, -5));
    }

    #[test]
    fn grid_rows_are_shaped_per_dialect() {
        let grid = Grid::new(-25.0, 27.0, 50.0, 72.0, 0.14, 0.08);
        let end = ts(1_700_000_000);
        let rows = vec![
            Row::new(vec![
                DBValue::Int(0),
                DBValue::Int(1),
                DBValue::Int(4),
                DBValue::Timestamp(ts(1_699_999_500)),
            ]),
            // out-of-range for a region grid: rx negative
            Row::new(vec![
                DBValue::Int(-1),
                DBValue::Int(1),
                DBValue::Int(9),
                DBValue::Timestamp(ts(1_699_999_500)),
            ]),
        ];
        let region_rows = build_grid_rows(&rows, &grid, end, false);
        // filter keeps only the in-range cell; y flipped to y_bin_count - ry
        assert_eq!(region_rows, vec![json!([0, 274, 4, -500])]);
        let global_rows = build_grid_rows(&rows, &grid, end, true);
        assert_eq!(
            global_rows,
            vec![json!([0, -2, 4, -500]), json!([-1, -2, 9, -500])]
        );
    }

    #[test]
    fn histogram_bins_counts() {
        let rows = vec![
            Row::new(vec![DBValue::Int(-2), DBValue::Int(1)]),
            Row::new(vec![DBValue::Int(0), DBValue::Int(7)]),
        ];
        // value_count = 15 / 5 = 3; interval -2 -> index 0, interval 0 -> index 2
        let h = build_histogram(&rows, 15, 5).unwrap();
        assert_eq!(h, vec![json!(1), json!(0), json!(7)]);
    }

    #[test]
    fn histogram_out_of_range_is_an_error() {
        let rows = vec![Row::new(vec![DBValue::Int(5), DBValue::Int(1)])];
        assert!(matches!(
            build_histogram(&rows, 15, 5),
            Err(ServiceError::HistogramIndex(5))
        ));
    }

    #[test]
    fn strikes_grid_response_is_built_with_all_keys() {
        let grid = Grid::new(-25.0, 27.0, 50.0, 72.0, 0.14017221762500753, 0.08865376938211966);
        let interval = create_time_interval_at(30, 0, fixed_now());
        let response = Service::<crate::metrics::NoopMetrics>::build_grid_response(
            vec![json!([0, 1, 4, -500])],
            vec![json!(0), json!(0), json!(0), json!(0), json!(0), json!(1)],
            &grid,
            &interval,
        );
        let obj = response.as_object().unwrap();
        let keys: Vec<&String> = obj.keys().collect();
        assert_eq!(
            keys,
            vec!["r", "xd", "yd", "x0", "y1", "xc", "yc", "t", "dt", "h"]
        );
        assert_eq!(obj["xd"].as_f64().unwrap(), 0.140172);
        assert_eq!(obj["yd"].as_f64().unwrap(), 0.088654);
assert_eq!(obj["x0"].as_f64().unwrap(), -25.0);
        assert_eq!(obj["y1"].as_f64().unwrap(), 72.0887);
        assert_eq!(obj["xc"].as_i64().unwrap(), 371);
        assert_eq!(obj["yc"].as_i64().unwrap(), 249);
        assert_eq!(obj["t"].as_str().unwrap(), "20231114T22:13:20");
        assert_eq!(obj["dt"].as_i64().unwrap(), 1800);
    }

    #[tokio::test]
    async fn jsonrpc_get_strikes_grid_serves_cached_and_reports_ratio() {
        let mut mock = MockExecutor::new();
        // region 1 grid row within range
        mock.add_rows(
            "TRUNC((ST_X",
            vec![Row::new(vec![
                DBValue::Int(0),
                DBValue::Int(1),
                DBValue::Int(4),
                DBValue::Timestamp(ts(1_699_999_500)),
            ])],
        );
        mock.add_rows("-extract( epoch", vec![]);
        let metrics = crate::metrics::RecordingMetrics::new();
        let service: Service<crate::metrics::RecordingMetrics> = Service::with_parts(
            Arc::new(mock),
            ServiceCache::new(),
            metrics,
            HashSet::new(),
        );
        let mut req = req_with("5.6.7.8");

        let _response = service
            .jsonrpc_get_strikes_grid(
                &mut req,
                &json!(30),
                &json!(10_000),
                &json!(0),
                &json!(1),
                &json!(0),
            )
            .await;
        let _response = service
            .jsonrpc_get_strikes_grid(
                &mut req,
                &json!(30),
                &json!(10_000),
                &json!(0),
                &json!(1),
                &json!(0),
            )
            .await;
        // second call hits the cache: second query count is 1, ratio 0.5
        assert_eq!(service.cache().strikes(0).get_size(), 1);
        assert!((service.cache().strikes(0).get_ratio() - 0.5).abs() < 1e-9);
        // metrics recorded after each call: the histogram of the (30-minute)
        // grid query, then the three strikes-grid lines.
        let recorded = service.metrics.lines();
        assert_eq!(
            recorded,
            vec![
                "histogram.cache_hits:0|g".to_string(),
                "histogram.size:1|g".to_string(),
                "strikes_grid.total_count:1|c".to_string(),
                "strikes_grid.total_count.1:1|c".to_string(),
                "strikes_grid.cache_hits:0|g".to_string(),
                "strikes_grid.total_count:1|c".to_string(),
                "strikes_grid.total_count.1:1|c".to_string(),
                "strikes_grid.cache_hits:0.5|g".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn jsonrpc_get_strikes_grid_blocks_by_default() {
        let service = Service::new(Arc::new(MockExecutor::new()));
        let mut req = Request::default();
        let response = service
            .jsonrpc_get_strikes_grid(&mut req, &json!(60), &json!(10_000), &json!(0), &json!(1), &json!(0))
            .await;
        assert_eq!(response, json!({}));
    }

    #[tokio::test]
    async fn jsonrpc_get_global_strikes_grid_blocks_below_25000() {
        let service = Service::new(Arc::new(MockExecutor::new()));
        let mut req = req_with("5.6.7.8");
        // gbl 10000 < 25000 is forbidden for the global endpoint
        let response = service
            .jsonrpc_get_global_strikes_grid(
                &mut req,
                &json!(60),
                &json!(10_000),
                &json!(0),
                &json!(0),
            )
            .await;
        assert_eq!(response, json!({}));
    }

    #[tokio::test]
    async fn jsonrpc_get_global_strikes_grid_serves_response() {
        let mut mock = MockExecutor::new();
        mock.add_rows(
            "ROUND((ST_X",
            vec![Row::new(vec![
                DBValue::Int(0),
                DBValue::Int(1),
                DBValue::Int(4),
                DBValue::Timestamp(ts(1_699_999_500)),
            ])],
        );
        mock.add_rows("-extract( epoch", vec![]);
        let service = Service::new(Arc::new(mock));
        let mut req = req_with("5.6.7.8");
        let response = service
            .jsonrpc_get_global_strikes_grid(
                &mut req,
                &json!(30),
                &json!(25_000),
                &json!(0),
                &json!(0),
            )
            .await;
        let obj = response.as_object().expect("grid response");
        for key in ["r", "xd", "yd", "x0", "y1", "xc", "yc", "t", "dt", "h"] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
        // global rows are not filtered and use the -ry-1 y translation
        let rows = obj["r"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0], json!(0));
        assert_eq!(rows[0][1], json!(-2));
        assert_eq!(rows[0][2], json!(4));
    }

    #[tokio::test]
    async fn jsonrpc_get_local_strikes_grid_serves_response() {
        let mut mock = MockExecutor::new();
        mock.add_rows(
            "TRUNC((ST_X",
            vec![Row::new(vec![
                DBValue::Int(0),
                DBValue::Int(1),
                DBValue::Int(4),
                DBValue::Timestamp(ts(1_699_999_500)),
            ])],
        );
        mock.add_rows("-extract( epoch", vec![]);
        let service = Service::new(Arc::new(mock));
        let mut req = req_with("5.6.7.8");
        let response = service
            .jsonrpc_get_local_strikes_grid(
                &mut req,
                &json!(5),
                &json!(5),
                &json!(10_000),
                &json!(30),
                &json!(0),
                &json!(0),
                &json!(5),
            )
            .await;
        let obj = response.as_object().expect("grid response");
        for key in ["r", "xd", "yd", "x0", "y1", "xc", "yc", "t", "dt", "h"] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
        // region-filtered local rows keep the strike count
        let rows = obj["r"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][2], json!(4));
    }

    #[tokio::test]
    async fn jsonrpc_get_strikes_grid_invalid_params_yields_empty_object() {
        let service = Service::new(Arc::new(MockExecutor::new()));
        let mut req = req_with("5.6.7.8");
        let response = service
            .jsonrpc_get_strikes_grid(&mut req, &json!("x"), &json!(10_000), &json!(0), &json!(1), &json!(0))
            .await;
        assert_eq!(response, json!({}));
    }
}