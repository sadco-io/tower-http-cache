//! Statistics collection for the admin API.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Global statistics collector.
pub struct GlobalStats {
    /// Total number of cache requests
    pub total_requests: AtomicU64,

    /// Total cache hits
    pub hits: AtomicU64,

    /// Total cache misses
    pub misses: AtomicU64,

    /// Total stale hits served
    pub stale_hits: AtomicU64,

    /// Total entries stored
    pub stores: AtomicU64,

    /// Total invalidations
    pub invalidations: AtomicU64,

    /// Per-key hit tracking for hot keys
    key_hits: Arc<DashMap<String, KeyHitStats>>,

    /// Uptime start
    pub uptime_start: SystemTime,
}

impl GlobalStats {
    /// Creates a new statistics collector.
    pub fn new() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            stale_hits: AtomicU64::new(0),
            stores: AtomicU64::new(0),
            invalidations: AtomicU64::new(0),
            key_hits: Arc::new(DashMap::new()),
            uptime_start: SystemTime::now(),
        }
    }

    /// Records a cache hit.
    pub fn record_hit(&self, key: &str) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        self.hits.fetch_add(1, Ordering::Relaxed);
        self.record_key_hit(key);
    }

    /// Records a cache miss.
    pub fn record_miss(&self, _key: &str) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        self.misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a stale hit.
    pub fn record_stale_hit(&self, key: &str) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        self.stale_hits.fetch_add(1, Ordering::Relaxed);
        self.record_key_hit(key);
    }

    /// Records a cache store operation.
    pub fn record_store(&self, _key: &str) {
        self.stores.fetch_add(1, Ordering::Relaxed);
    }

    /// Records an invalidation.
    pub fn record_invalidation(&self) {
        self.invalidations.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a hit for a specific key (for hot key tracking).
    fn record_key_hit(&self, key: &str) {
        self.key_hits
            .entry(key.to_string())
            .or_insert_with(KeyHitStats::new)
            .increment();
    }

    /// Returns the top N most frequently accessed keys.
    pub fn hot_keys(&self, limit: usize) -> Vec<HotKeyInfo> {
        let mut keys: Vec<_> = self
            .key_hits
            .iter()
            .map(|entry| HotKeyInfo {
                key: entry.key().clone(),
                hits: entry.value().hits(),
                last_accessed: entry.value().last_accessed,
            })
            .collect();

        keys.sort_by(|a, b| b.hits.cmp(&a.hits));
        keys.truncate(limit);
        keys
    }

    /// Returns the current statistics as a serializable struct.
    pub fn snapshot(&self) -> StatsSnapshot {
        let total = self.total_requests.load(Ordering::Relaxed);
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);

        let hit_rate = if total > 0 {
            hits as f64 / total as f64
        } else {
            0.0
        };

        let miss_rate = if total > 0 {
            misses as f64 / total as f64
        } else {
            0.0
        };

        let uptime = SystemTime::now()
            .duration_since(self.uptime_start)
            .unwrap_or_default()
            .as_secs();

        StatsSnapshot {
            total_requests: total,
            hits,
            misses,
            stale_hits: self.stale_hits.load(Ordering::Relaxed),
            stores: self.stores.load(Ordering::Relaxed),
            invalidations: self.invalidations.load(Ordering::Relaxed),
            hit_rate,
            miss_rate,
            uptime_seconds: uptime,
        }
    }

    /// Resets all statistics.
    pub fn reset(&self) {
        self.total_requests.store(0, Ordering::Relaxed);
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
        self.stale_hits.store(0, Ordering::Relaxed);
        self.stores.store(0, Ordering::Relaxed);
        self.invalidations.store(0, Ordering::Relaxed);
        self.key_hits.clear();
    }
}

impl Default for GlobalStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-key hit statistics.
struct KeyHitStats {
    hits: AtomicU64,
    last_accessed: SystemTime,
}

impl KeyHitStats {
    fn new() -> Self {
        Self {
            hits: AtomicU64::new(0),
            last_accessed: SystemTime::now(),
        }
    }

    fn increment(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
    }

    fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }
}

/// Information about a hot key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotKeyInfo {
    pub key: String,
    pub hits: u64,
    #[serde(serialize_with = "serialize_system_time")]
    pub last_accessed: SystemTime,
}

/// Snapshot of current statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsSnapshot {
    pub total_requests: u64,
    pub hits: u64,
    pub misses: u64,
    pub stale_hits: u64,
    pub stores: u64,
    pub invalidations: u64,
    pub hit_rate: f64,
    pub miss_rate: f64,
    pub uptime_seconds: u64,
}

/// Helper to serialize SystemTime as ISO 8601.
fn serialize_system_time<S>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = duration.as_secs();
    let timestamp =
        chrono::DateTime::from_timestamp(secs as i64, 0).unwrap_or(chrono::DateTime::UNIX_EPOCH);
    serializer.serialize_str(&timestamp.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_stats_initial_state() {
        let stats = GlobalStats::new();
        let snapshot = stats.snapshot();

        assert_eq!(snapshot.total_requests, 0);
        assert_eq!(snapshot.hits, 0);
        assert_eq!(snapshot.misses, 0);
        assert_eq!(snapshot.hit_rate, 0.0);
    }

    #[test]
    fn global_stats_record_operations() {
        let stats = GlobalStats::new();

        stats.record_hit("key1");
        stats.record_miss("key2");
        stats.record_stale_hit("key3");
        stats.record_store("key4");
        stats.record_invalidation();

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.total_requests, 3); // hit + miss + stale_hit
        assert_eq!(snapshot.hits, 1);
        assert_eq!(snapshot.misses, 1);
        assert_eq!(snapshot.stale_hits, 1);
        assert_eq!(snapshot.stores, 1);
        assert_eq!(snapshot.invalidations, 1);
    }

    #[test]
    fn global_stats_hit_rate_calculation() {
        let stats = GlobalStats::new();

        stats.record_hit("key1");
        stats.record_hit("key2");
        stats.record_miss("key3");

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.total_requests, 3);
        assert!((snapshot.hit_rate - 0.666).abs() < 0.01);
        assert!((snapshot.miss_rate - 0.333).abs() < 0.01);
    }

    #[test]
    fn global_stats_hot_keys() {
        let stats = GlobalStats::new();

        // Record hits with different frequencies
        for _ in 0..10 {
            stats.record_hit("hot_key");
        }
        for _ in 0..3 {
            stats.record_hit("warm_key");
        }
        stats.record_hit("cold_key");

        let hot_keys = stats.hot_keys(2);
        assert_eq!(hot_keys.len(), 2);
        assert_eq!(hot_keys[0].key, "hot_key");
        assert_eq!(hot_keys[0].hits, 10);
        assert_eq!(hot_keys[1].key, "warm_key");
        assert_eq!(hot_keys[1].hits, 3);
    }

    #[test]
    fn global_stats_reset() {
        let stats = GlobalStats::new();

        stats.record_hit("key1");
        stats.record_miss("key2");

        stats.reset();

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.total_requests, 0);
        assert_eq!(snapshot.hits, 0);
        assert_eq!(snapshot.misses, 0);
    }

    #[test]
    fn stats_snapshot_serialization() {
        let snapshot = StatsSnapshot {
            total_requests: 100,
            hits: 80,
            misses: 20,
            stale_hits: 5,
            stores: 90,
            invalidations: 10,
            hit_rate: 0.8,
            miss_rate: 0.2,
            uptime_seconds: 3600,
        };

        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains("\"total_requests\":100"));
        assert!(json.contains("\"hit_rate\":0.8"));
    }
}
