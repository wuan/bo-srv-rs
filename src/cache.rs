//! Service-side response caching, ported from `blitzortung/cache.py`
//! (`ObjectCache`) and `blitzortung/service/cache.py` (`ServiceCache`).
//!
//! The Python layer caches the *result* of a deferred producer keyed by
//! `(creator, args..., separator, sorted kwargs)` or an explicit
//! `cache_key`; the envelope (pre-1.0 array / v1 / v2 dict) is re-rendered
//! per request, so nobody caches an envelope.  This port mirrors that: the
//! service layer builds a stable string key and caches the raw result
//! [`serde_json::Value`]; every request renders its own envelope.

use std::sync::Mutex;

use serde_json::Value;

/// Wall-clock seconds (float), matching `time.time()`.
fn now_seconds() -> f64 {
    let now = chrono::Utc::now();
    now.timestamp() as f64 + f64::from(now.timestamp_subsec_nanos()) / 1e9
}

/// A single cache entry (payload + integral expiry second).
struct CacheEntry {
    payload: Value,
    expires: i64,
}

impl CacheEntry {
    /// `CacheEntry.is_valid(current_time)`: `current_time < expiry_time`.
    fn is_valid(&self, current_time: i64) -> bool {
        current_time < self.expires
    }
}

#[derive(Default)]
struct Inner {
    cache: std::collections::HashMap<String, CacheEntry>,
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
    fn get_at(&self, key: &str, creator: impl FnOnce() -> Value, now: f64) -> Value {
        let mut inner = self.lock();

        if let Some(period) = self.cleanup_period {
            if now > inner.last_cleanup + period {
                inner.clean_expired(now);
                inner.last_cleanup = now;
            }
        }

        inner.total_count += 1;
        let current_time = now as i64;

        // `Some(Some(payload))` = valid cached entry; `Some(None)` = an entry
        // exists but is expired (falls through to re-create); `None` = no
        // entry (the size-eviction branch applies).
        let cached = inner
            .cache
            .get(key)
            .map(|entry| entry.is_valid(current_time).then(|| entry.payload.clone()));
        match cached {
            Some(Some(payload)) => {
                if self.size.is_some() {
                    inner.track_usage(key);
                }
                inner.total_hit_count += 1;
                return payload;
            }
            Some(None) => {}
            None => {
                if let Some(size) = self.size {
                    if inner.cache.len() >= size {
                        inner.remove_oldest_entry();
                    }
                }
            }
        }

        let payload = creator();
        let entry = CacheEntry {
            expires: current_time + self.ttl_seconds,
            payload: payload.clone(),
        };
        inner.track_usage(key);
        inner.cache.insert(key.to_string(), entry);
        payload
    }

    /// Fetch `key`, computing the payload with `creator` on a miss
    /// (`ObjectCache.get`).
    pub fn get(&self, key: &str, creator: impl FnOnce() -> Value) -> Value {
        self.get_at(key, creator, now_seconds())
    }

    /// [`ObjectCache::get`] for producers that can fail: on `Err` nothing is
    /// cached and the error is returned (mirrors the Python behaviour, where
    /// an errored Deferred is neither cached nor returned as a response).
    pub fn get_result<F, E>(&self, key: &str, creator: F) -> Result<Value, E>
    where
        F: FnOnce() -> Result<Value, E>,
    {
        let now = now_seconds();
        let mut inner = self.lock();

        if let Some(period) = self.cleanup_period {
            if now > inner.last_cleanup + period {
                inner.clean_expired(now);
                inner.last_cleanup = now;
            }
        }

        inner.total_count += 1;
        let current_time = now as i64;

        let cached = inner
            .cache
            .get(key)
            .map(|entry| entry.is_valid(current_time).then(|| entry.payload.clone()));
        match cached {
            Some(Some(payload)) => {
                if self.size.is_some() {
                    inner.track_usage(key);
                }
                inner.total_hit_count += 1;
                return Ok(payload);
            }
            Some(None) => {}
            None => {
                if let Some(size) = self.size {
                    if inner.cache.len() >= size {
                        inner.remove_oldest_entry();
                    }
                }
            }
        }

        let payload = creator()?;
        let entry = CacheEntry {
            expires: current_time + self.ttl_seconds,
            payload: payload.clone(),
        };
        inner.track_usage(key);
        inner.cache.insert(key.to_string(), entry);
        Ok(payload)
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

    /// A panic while another thread holds the cache lock must not cascade:
    /// the lock is poisoned but the cache still works.
    #[test]
    fn poisoned_mutex_does_not_panic_subsequent_access() {
        let cache = ObjectCache::new(60, None, None);

        // Poison the mutex by panicking a closure that runs under the lock.
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = cache.lock();
            panic!("deliberate panic under the cache lock");
        }));
        assert!(poisoned.is_err());

        // Accessing the cache must not panic now.
        assert_eq!(cache.get_size(), 0);
        let value = cache.get("k", || json!({"v": 1}));
        assert_eq!(value, json!({"v": 1}));
        assert_eq!(cache.get_size(), 1);
        assert_eq!(cache.get_ratio(), 0.0); // one get, zero hits
    }

    #[test]
    fn misses_compute_and_hits_are_cached() {
        let cache = ObjectCache::new(60, None, None);
        let creator_count = std::cell::Cell::new(0);
        let compute = || {
            creator_count.set(creator_count.get() + 1);
            json!({"value": creator_count.get()})
        };
        let key = "k";

        let now = 1_000.0;
        let first = cache.get_at(key, compute, now);
        assert_eq!(first, json!({"value": 1}));
        let second = cache.get_at(key, compute, now + 10.0);
        assert_eq!(second, json!({"value": 1}));
        assert_eq!(creator_count.get(), 1);
        assert_eq!(cache.get_ratio(), 0.5);
        assert_eq!(cache.get_size(), 1);
    }

    #[test]
    fn expired_entries_are_recomputed() {
        let cache = ObjectCache::new(20, None, None);
        let mut created = 0;
        let now = 1_000.0;
        let first = cache.get_at("k", || {
            created += 1;
            json!(created)
        }, now);
        assert_eq!(first, json!(1));
        // after the TTL, the entry is stale even within the same second
        let second = cache.get_at("k", || {
            created += 1;
            json!(created)
        }, 1_020.0);
        assert_eq!(second, json!(2));
    }

    #[test]
    fn lru_size_limit_evicts_oldest() {
        // size 2: the oldest key is evicted before a new entry is inserted
        let cache = ObjectCache::new(60, Some(2), None);
        let now = 1_000.0;

        assert_eq!(cache.get_at("a", || json!(1), now), json!(1));
        assert_eq!(cache.get_at("b", || json!(2), now), json!(2));
        // touch "a", pushing it behind "b"
        assert_eq!(cache.get_at("a", || json!(99), now), json!(1));
        // insert "c": cache full (2 >= 2) -> evict oldest ("b")
        assert_eq!(cache.get_at("c", || json!(3), now), json!(3));
        assert_eq!(cache.get_size(), 2);
        // "b" was evicted; re-fetching it evicts the oldest ("a") again
        assert_eq!(cache.get_at("b", || json!(22), now), json!(22));
        assert_eq!(cache.get_size(), 2);
        // "a" was evicted in turn, so it is recomputed
        assert_eq!(cache.get_at("a", || json!(77), now), json!(77));
        assert_eq!(cache.get_at("a", || json!(78), now), json!(77)); // now cached
    }

    #[test]
    fn cleanup_removes_expired_entries() {
        let cache = ObjectCache::new(20, None, Some(300.0));
        let now = 1_000.0;
        assert_eq!(cache.get_at("k", || json!(1), now), json!(1));
        // same-second get after the cleanup period: expired removed, recompute
        let value = cache.get_at("k", || json!(2), now + 300.1);
        assert_eq!(value, json!(2));
        assert_eq!(cache.get_size(), 1);
    }

    #[test]
    fn get_result_does_not_cache_errors() {
        let cache = ObjectCache::new(60, None, None);
        let result: Result<Value, String> =
            cache.get_result("k", || Err("boom".to_string()));
        assert!(result.is_err());
        assert_eq!(cache.get_size(), 0);
        // a later successful production stores the value
        let ok: Result<Value, String> = cache.get_result("k", || Ok(json!(42)));
        assert_eq!(ok.unwrap(), json!(42));
        assert_eq!(cache.get_size(), 1);
        let hit: Result<Value, String> = cache.get_result("k", || Ok(json!(99)));
        assert_eq!(hit.unwrap(), json!(42));
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