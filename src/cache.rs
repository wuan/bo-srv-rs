//! Service-side response caching, ported from `blitzortung/cache.py`
//! (`ObjectCache`) and `blitzortung/service/cache.py` (`ServiceCache`).
//!
//! The Python layer caches the *result* of a deferred producer keyed by
//! `(creator, args..., separator, sorted kwargs)` or an explicit `cache_key`;
//! the envelope (pre-1.0 array / v1 / v2 dict) is re-rendered per request, so
//! nobody caches an envelope.  This port mirrors that: the service layer builds
//! a stable string key and caches the raw result [`serde_json::Value`]; every
//! request renders its own envelope.
//!
//! # In-flight ("single-flight") coalescing
//!
//! Producers are asynchronous — they run database queries the service handler
//! `.await`s.  A miss therefore stores an *in-flight computation*, not just a
//! finished value: the first request claims the key and drives the producer,
//! and every concurrent request for the same key awaits the same shared cell
//! (see [`ObjectCache::get_result`]).  This removes the "thundering herd" of
//! duplicate queries on a cold-cache burst while staying lock-free on the
//! async path (the map lock is only held for a short synchronous lookup).

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::OnceCell;

/// A producer or database error as it is stored in the cache: cloneable and
/// type-erased, so one cache can hold entries produced by different callers.
pub type CacheError = Arc<dyn std::error::Error + Send + Sync>;

/// Wall-clock seconds (float), matching `time.time()`.
fn now_seconds() -> f64 {
    let now = chrono::Utc::now();
    now.timestamp() as f64 + f64::from(now.timestamp_subsec_nanos()) / 1e9
}

/// A single cache slot: the integral expiry second plus the shared, lazily
/// completed result.  Concurrent readers await the same [`OnceCell`], so the
/// producer runs at most once per key (per generation).
struct CacheEntry {
    expires: i64,
    result: OnceCell<Result<Value, CacheError>>,
}

impl CacheEntry {
    fn new(expires: i64) -> Self {
        CacheEntry {
            expires,
            result: OnceCell::new(),
        }
    }

    /// `CacheEntry.is_valid(current_time)`: `current_time < expiry_time`.
    fn is_valid(&self, current_time: i64) -> bool {
        current_time < self.expires
    }
}

#[derive(Default)]
struct Inner {
    cache: std::collections::HashMap<String, Arc<CacheEntry>>,
    /// LRU position list (front = least recently used), matching the Python
    /// ordered `keys` dict.
    keys: Vec<String>,
    last_cleanup: f64,
    total_count: u64,
    total_hit_count: u64,
}

impl Inner {
    fn remove_oldest_entry(&mut self) {
        if let Some(key) = self.keys.first().cloned() {
            self.keys.remove(0);
            self.cache.remove(&key);
        }
    }

    fn track_usage(&mut self, key: &str) {
        if let Some(pos) = self.keys.iter().position(|k| k == key) {
            self.keys.remove(pos);
        }
        self.keys.push(key.to_string());
    }

    fn clean_expired(&mut self, now: f64) {
        let expired: Vec<String> = self
            .cache
            .iter()
            .filter(|(_, entry)| !entry.is_valid(now as i64))
            .map(|(key, _)| key.clone())
            .collect();
        for key in expired {
            if let Some(pos) = self.keys.iter().position(|k| *k == key) {
                self.keys.remove(pos);
            }
            self.cache.remove(&key);
        }
    }
}

/// TTL-based cache with an optional LRU size limit and optional periodic
/// cleanup (ported from `blitzortung.cache.ObjectCache`).
///
/// Misses store an in-flight computation shared by all concurrent callers
/// ("single-flight"): the first caller runs the producer, the others await its
/// result.  Failed computations are **not** retained, so a transient database
/// error does not poison the key for the TTL window.
pub struct ObjectCache {
    ttl_seconds: i64,
    size: Option<usize>,
    cleanup_period: Option<f64>,
    inner: Mutex<Inner>,
}

impl ObjectCache {
    pub fn new(ttl_seconds: i64, size: Option<usize>, cleanup_period: Option<f64>) -> Self {
        ObjectCache {
            ttl_seconds,
            size,
            cleanup_period,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Lock the cache, tolerating a poisoned mutex so that a panic occurring
    /// while another task held the lock cannot cascade-panic the whole server.
    /// The cache only reads/writes owned data, so it is safe to use the
    /// (possibly inconsistent) inner state after a poison.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Fetch `key`, computing the payload with `creator` on a miss
    /// (`ObjectCache.get`).  `now` is injectable for tests.
    #[cfg(test)]
    async fn get_at<F, Fut>(&self, key: &str, creator: F, now: f64) -> Value
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Value, CacheError>>,
    {
        match self.get_result_at(key, creator, now).await {
            Ok(payload) => payload,
            Err(_) => Value::Null,
        }
    }

    /// Fetch `key`, computing the payload with `creator` on a miss
    /// (`ObjectCache.get`).
    pub async fn get<F, Fut>(&self, key: &str, creator: F) -> Value
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Value, CacheError>>,
    {
        match self.get_result(key, creator).await {
            Ok(payload) => payload,
            Err(_) => Value::Null,
        }
    }

    /// Fetch `key`, computing the payload with `creator` on a miss.
    ///
    /// On a miss the *first* caller inserts an empty shared cell and drives
    /// `creator`; any concurrent caller for the same key awaits that same cell
    /// instead of starting a second producer.  On `Err` the entry is removed so
    /// nothing is cached (mirrors the Python behaviour, where an errored
    /// Deferred is neither cached nor returned as a response).
    pub async fn get_result<F, Fut>(&self, key: &str, creator: F) -> Result<Value, CacheError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Value, CacheError>>,
    {
        self.get_result_at(key, creator, now_seconds()).await
    }

    /// [`ObjectCache::get_result`] with an injectable clock.
    async fn get_result_at<F, Fut>(
        &self,
        key: &str,
        creator: F,
        now: f64,
    ) -> Result<Value, CacheError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Value, CacheError>>,
    {
        let current_time = now as i64;

        // Short synchronous section: cleanup, lookup and (on a miss) claiming
        // the key.  The lock is released before any await.
        let (entry, hit) = {
            let mut inner = self.lock();

            if let Some(period) = self.cleanup_period {
                if now > inner.last_cleanup + period {
                    inner.clean_expired(now);
                    inner.last_cleanup = now;
                }
            }

            inner.total_count += 1;

            // A valid entry (with or without a completed result yet) is reused;
            // an expired one is replaced.
            let valid = inner
                .cache
                .get(key)
                .filter(|entry| entry.is_valid(current_time))
                .cloned();
            match valid {
                Some(entry) => {
                    if self.size.is_some() {
                        inner.track_usage(key);
                    }
                    if entry.result.initialized() {
                        inner.total_hit_count += 1;
                    }
                    (entry, true)
                }
                None => {
                    if let Some(size) = self.size {
                        if inner.cache.len() >= size {
                            inner.remove_oldest_entry();
                        }
                    }
                    let entry = Arc::new(CacheEntry::new(current_time + self.ttl_seconds));
                    inner.track_usage(key);
                    inner.cache.insert(key.to_string(), entry.clone());
                    (entry, false)
                }
            }
        };

        if hit && entry.result.initialized() {
            // Fast path: the value is already computed.
            return entry
                .result
                .get()
                .cloned()
                .expect("initialized cell always has a value");
        }

        // Drive (miss) or await (hit but still in flight) the shared cell.
        let result = entry.result.get_or_init(creator).await.clone();

        if result.is_err() {
            // Do not cache failures; drop the entry only if it is still the one
            // we used (a newer generation may already have replaced it).
            let mut inner = self.lock();
            let remove = inner
                .cache
                .get(key)
                .is_some_and(|current| Arc::ptr_eq(current, &entry));
            if remove {
                inner.cache.remove(key);
                if let Some(pos) = inner.keys.iter().position(|k| k == key) {
                    inner.keys.remove(pos);
                }
            }
        }

        result
    }

    /// `ObjectCache.get_ratio`: hits / total gets, `0.0` when there were no
    /// hits.
    pub fn get_ratio(&self) -> f64 {
        let inner = self.lock();
        if inner.total_hit_count == 0 {
            0.0
        } else {
            inner.total_hit_count as f64 / inner.total_count as f64
        }
    }

    /// `ObjectCache.get_size`: number of stored entries.
    pub fn get_size(&self) -> usize {
        self.lock().cache.len()
    }

    /// `ObjectCache.get_time_to_live`.
    pub fn get_time_to_live(&self) -> i64 {
        self.ttl_seconds
    }

    /// `ObjectCache.clear`.
    pub fn clear(&self) {
        let mut inner = self.lock();
        inner.cache.clear();
        inner.keys.clear();
        inner.total_count = 0;
        inner.total_hit_count = 0;
        inner.last_cleanup = 0.0;
    }
}

/// The service-wide cache layout
/// (`blitzortung.service.cache.ServiceCache`): short TTL (20 s) for the
/// current minute, long TTL (60 s) for history; the local grids additionally
/// cap their entry count (100 current / 400 history).
pub struct ServiceCache {
    strikes_grid: ObjectCache,
    strikes_history_grid: ObjectCache,
    global_strikes_grid: ObjectCache,
    global_strikes_history_grid: ObjectCache,
    local_strikes_grid: ObjectCache,
    local_strikes_history_grid: ObjectCache,
    /// Shared histogram cache (long TTL, uncapped).
    pub histogram: ObjectCache,
}

impl ServiceCache {
    const CLEANUP_PERIOD: f64 = 300.0;
    const TTL_SHORT: i64 = 20;
    const TTL_LONG: i64 = 60;
    const LOCAL_CACHE_SIZE_CURRENT: usize = 100;
    const LOCAL_CACHE_SIZE_HISTORY: usize = 400;

    pub fn new() -> Self {
        Self::new_with_cleanup(Some(Self::CLEANUP_PERIOD))
    }

    /// Testable variant: `cleanup_period` can be disabled.
    pub fn new_with_cleanup(cleanup_period: Option<f64>) -> Self {
        ServiceCache {
            strikes_grid: ObjectCache::new(Self::TTL_SHORT, None, cleanup_period),
            strikes_history_grid: ObjectCache::new(Self::TTL_LONG, None, cleanup_period),
            global_strikes_grid: ObjectCache::new(Self::TTL_SHORT, None, cleanup_period),
            global_strikes_history_grid: ObjectCache::new(Self::TTL_LONG, None, cleanup_period),
            local_strikes_grid: ObjectCache::new(
                Self::TTL_SHORT,
                Some(Self::LOCAL_CACHE_SIZE_CURRENT),
                cleanup_period,
            ),
            local_strikes_history_grid: ObjectCache::new(
                Self::TTL_LONG,
                Some(Self::LOCAL_CACHE_SIZE_HISTORY),
                cleanup_period,
            ),
            histogram: ObjectCache::new(Self::TTL_LONG, None, cleanup_period),
        }
    }

    /// `ServiceCache.strikes(minute_offset)`: the current-minute cache for
    /// offsets of 0, the history cache otherwise.
    pub fn strikes(&self, minute_offset: i64) -> &ObjectCache {
        if minute_offset == 0 {
            &self.strikes_grid
        } else {
            &self.strikes_history_grid
        }
    }

    pub fn global_strikes(&self, minute_offset: i64) -> &ObjectCache {
        if minute_offset == 0 {
            &self.global_strikes_grid
        } else {
            &self.global_strikes_history_grid
        }
    }

    pub fn local_strikes(&self, minute_offset: i64) -> &ObjectCache {
        if minute_offset == 0 {
            &self.local_strikes_grid
        } else {
            &self.local_strikes_history_grid
        }
    }
}

impl Default for ServiceCache {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A cloneable error for producer failures.
    fn err(message: &str) -> CacheError {
        Arc::new(std::io::Error::other(message.to_string()))
    }

    #[tokio::test]
    async fn misses_compute_and_hits_are_cached() {
        let cache = ObjectCache::new(60, None, None);
        let creator_count = AtomicUsize::new(0);
        let compute = || async {
            let n = creator_count.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(json!({"value": n}))
        };

        let now = 1_000.0;
        let first = cache.get_at("k", compute, now).await;
        assert_eq!(first, json!({"value": 1}));
        let second = cache.get_at("k", compute, now + 10.0).await;
        assert_eq!(second, json!({"value": 1}));
        assert_eq!(creator_count.load(Ordering::SeqCst), 1);
        assert_eq!(cache.get_ratio(), 0.5);
        assert_eq!(cache.get_size(), 1);
    }

    #[tokio::test]
    async fn expired_entries_are_recomputed() {
        let cache = ObjectCache::new(20, None, None);
        let mut created = 0;
        let now = 1_000.0;
        let first = cache
            .get_at(
                "k",
                || async {
                    created += 1;
                    Ok(json!(created))
                },
                now,
            )
            .await;
        assert_eq!(first, json!(1));
        // after the TTL, the entry is stale even within the same second
        let second = cache
            .get_at(
                "k",
                || async {
                    created += 1;
                    Ok(json!(created))
                },
                1_020.0,
            )
            .await;
        assert_eq!(second, json!(2));
    }

    #[tokio::test]
    async fn lru_size_limit_evicts_oldest() {
        // size 2: the oldest key is evicted before a new entry is inserted
        let cache = ObjectCache::new(60, Some(2), None);
        let now = 1_000.0;

        assert_eq!(
            cache.get_at("a", || async { Ok(json!(1)) }, now).await,
            json!(1)
        );
        assert_eq!(
            cache.get_at("b", || async { Ok(json!(2)) }, now).await,
            json!(2)
        );
        // touch "a", pushing it behind "b"
        assert_eq!(
            cache.get_at("a", || async { Ok(json!(99)) }, now).await,
            json!(1)
        );
        // insert "c": cache full (2 >= 2) -> evict oldest ("b")
        assert_eq!(
            cache.get_at("c", || async { Ok(json!(3)) }, now).await,
            json!(3)
        );
        assert_eq!(cache.get_size(), 2);
        // "b" was evicted; re-fetching it evicts the oldest ("a") again
        assert_eq!(
            cache.get_at("b", || async { Ok(json!(22)) }, now).await,
            json!(22)
        );
        assert_eq!(cache.get_size(), 2);
        // "a" was evicted in turn, so it is recomputed
        assert_eq!(
            cache.get_at("a", || async { Ok(json!(77)) }, now).await,
            json!(77)
        );
        assert_eq!(
            cache.get_at("a", || async { Ok(json!(78)) }, now).await,
            json!(77)
        );
    }

    #[tokio::test]
    async fn cleanup_removes_expired_entries() {
        let cache = ObjectCache::new(20, None, Some(300.0));
        let now = 1_000.0;
        assert_eq!(
            cache.get_at("k", || async { Ok(json!(1)) }, now).await,
            json!(1)
        );
        // same-second get after the cleanup period: expired removed, recompute
        let value = cache
            .get_at("k", || async { Ok(json!(2)) }, now + 300.1)
            .await;
        assert_eq!(value, json!(2));
        assert_eq!(cache.get_size(), 1);
    }

    #[tokio::test]
    async fn get_result_does_not_cache_errors() {
        let cache = ObjectCache::new(60, None, None);
        let result = cache.get_result("k", || async { Err(err("boom")) }).await;
        assert!(result.is_err());
        assert_eq!(cache.get_size(), 0);
        // a later successful production stores the value
        let ok = cache.get_result("k", || async { Ok(json!(42)) }).await;
        assert_eq!(ok.unwrap(), json!(42));
        assert_eq!(cache.get_size(), 1);
        let hit = cache.get_result("k", || async { Ok(json!(99)) }).await;
        assert_eq!(hit.unwrap(), json!(42));
    }

    /// Concurrent misses on the same key share one in-flight computation
    /// ("single-flight"): the producer runs once and every caller gets the
    /// result.
    #[tokio::test]
    async fn concurrent_misses_coalesce_into_one_producer() {
        let cache = Arc::new(ObjectCache::new(60, None, None));
        let calls = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..64 {
            let cache = cache.clone();
            let calls = calls.clone();
            handles.push(tokio::spawn(async move {
                cache
                    .get_result("k", || async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(json!(7))
                    })
                    .await
                    .unwrap()
            }));
        }
        for handle in handles {
            assert_eq!(handle.await.unwrap(), json!(7));
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the producer must run once for a coalesced burst"
        );
    }

    /// The producer runs without the cache lock held, so a slow producer cannot
    /// stall other requests on the mutex (the original freeze).
    #[tokio::test]
    async fn creator_runs_without_holding_the_cache_lock() {
        let cache = Arc::new(ObjectCache::new(60, None, None));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();

        let producer_cache = cache.clone();
        let producer = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(producer_cache.get_result("k", || async {
                entered_tx.send(()).unwrap();
                // Block like a database query would, until the test releases us.
                release_rx.recv().unwrap();
                Ok(json!(1))
            }))
        });

        // Once the creator is running, touch the cache from another thread.
        // If the lock were held across the creator this would block forever.
        entered_rx.recv().unwrap();
        assert_eq!(cache.get_size(), 1);
        assert_eq!(cache.get_ratio(), 0.0);

        release_tx.send(()).unwrap();
        assert_eq!(producer.join().unwrap().unwrap(), json!(1));
    }

    /// A panic while another thread holds the cache lock must not cascade:
    /// the lock is poisoned but the cache still works.
    #[tokio::test]
    async fn poisoned_mutex_does_not_panic_subsequent_access() {
        let cache = Arc::new(ObjectCache::new(60, None, None));
        assert_eq!(
            cache
                .get_result("k", || async { Ok(json!(1)) })
                .await
                .unwrap(),
            json!(1)
        );

        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = cache.lock();
            panic!("boom");
        }));
        assert!(poisoned.is_err());

        // The cache still serves without panicking.
        assert_eq!(
            cache
                .get_result("k", || async { Ok(json!(2)) })
                .await
                .unwrap(),
            json!(1)
        );
    }

    #[test]
    fn service_cache_selects_by_minute_offset() {
        let cache = ServiceCache::new();
        assert_eq!(cache.strikes(0).get_time_to_live(), 20);
        assert_eq!(cache.strikes(-5).get_time_to_live(), 60);
        assert_eq!(cache.local_strikes(0).get_time_to_live(), 20);
        assert_eq!(cache.local_strikes(-5).get_time_to_live(), 60);
        assert_eq!(cache.global_strikes(0).get_time_to_live(), 20);
        assert_eq!(cache.histogram.get_time_to_live(), 60);
    }
}
