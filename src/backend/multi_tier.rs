//! Multi-tier caching backend with automatic promotion.
//!
//! This module implements a two-tier caching architecture where frequently
//! accessed entries are automatically promoted from a slower L2 cache to
//! a faster L1 cache based on configurable promotion strategies.

use dashmap::DashMap;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{CacheBackend, CacheEntry, CacheRead};
use crate::error::CacheError;

/// Strategy for promoting entries from L2 to L1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PromotionStrategy {
    /// Promote after a fixed number of hits
    HitCount { threshold: u64 },

    /// Promote based on hit rate over time window
    HitRate { threshold_per_minute: u64 },
}

impl Default for PromotionStrategy {
    fn default() -> Self {
        Self::HitCount { threshold: 3 }
    }
}

/// Statistics for a cache tier.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TierStats {
    pub l1_hits: u64,
    pub l2_hits: u64,
    pub misses: u64,
    pub promotions: u64,
    pub writes: u64,
}

#[derive(Default)]
struct AtomicTierStats {
    l1_hits: AtomicU64,
    l2_hits: AtomicU64,
    misses: AtomicU64,
    promotions: AtomicU64,
    writes: AtomicU64,
}

impl AtomicTierStats {
    fn snapshot(&self) -> TierStats {
        TierStats {
            l1_hits: self.l1_hits.load(Ordering::Relaxed),
            l2_hits: self.l2_hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            promotions: self.promotions.load(Ordering::Relaxed),
            writes: self.writes.load(Ordering::Relaxed),
        }
    }
}

/// Configuration for multi-tier caching.
#[derive(Debug, Clone)]
pub struct MultiTierConfig {
    /// Strategy for promoting entries from L2 to L1
    pub promotion_strategy: PromotionStrategy,

    /// Whether to write to both tiers on set (true) or L2 only (false)
    pub write_through: bool,

    /// Don't store entries larger than this in L1 (default: 256KB)
    /// Large entries are only stored in L2 to prevent L1 pollution
    pub max_l1_entry_size: Option<usize>,
}

impl Default for MultiTierConfig {
    fn default() -> Self {
        Self {
            promotion_strategy: PromotionStrategy::default(),
            write_through: true,
            max_l1_entry_size: Some(256 * 1024), // 256KB
        }
    }
}

/// Per-key statistics for promotion tracking.
struct KeyStats {
    l2_hits: AtomicU64,
    recent_hits: Mutex<VecDeque<Instant>>,
}

impl KeyStats {
    fn new() -> Self {
        Self {
            l2_hits: AtomicU64::new(0),
            recent_hits: Mutex::new(VecDeque::new()),
        }
    }

    fn prune_window(recent: &mut VecDeque<Instant>, now: Instant) {
        let cutoff = now.checked_sub(Duration::from_secs(60));
        while let Some(&front) = recent.front() {
            if cutoff.is_some_and(|c| front < c) {
                recent.pop_front();
            } else {
                break;
            }
        }
    }

    fn record_hit(&self) -> u64 {
        let now = Instant::now();
        let mut recent = self.recent_hits.lock().unwrap_or_else(|e| e.into_inner());
        Self::prune_window(&mut recent, now);
        recent.push_back(now);
        self.l2_hits.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn reset(&self) {
        self.l2_hits.store(0, Ordering::Relaxed);
        self.recent_hits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    fn hits(&self) -> u64 {
        self.l2_hits.load(Ordering::Relaxed)
    }

    fn hits_in_last_minute(&self) -> u64 {
        let now = Instant::now();
        let mut recent = self.recent_hits.lock().unwrap_or_else(|e| e.into_inner());
        Self::prune_window(&mut recent, now);
        recent.len() as u64
    }
}

/// Multi-tier cache backend combining fast L1 and persistent L2.
///
/// The multi-tier backend automatically promotes frequently accessed entries
/// from L2 to L1 based on the configured promotion strategy.
#[derive(Clone)]
pub struct MultiTierBackend<L1, L2> {
    l1: L1,
    l2: L2,
    config: MultiTierConfig,
    key_stats: Arc<DashMap<String, Arc<KeyStats>>>,
    tier_stats: Arc<AtomicTierStats>,
}

impl<L1, L2> MultiTierBackend<L1, L2>
where
    L1: CacheBackend,
    L2: CacheBackend,
{
    /// Creates a new multi-tier backend with default configuration.
    pub fn new(l1: L1, l2: L2) -> Self {
        Self {
            l1,
            l2,
            config: MultiTierConfig::default(),
            key_stats: Arc::new(DashMap::new()),
            tier_stats: Arc::new(AtomicTierStats::default()),
        }
    }

    /// Creates a builder for configuring the multi-tier backend.
    pub fn builder() -> MultiTierBuilder<L1, L2> {
        MultiTierBuilder::new()
    }

    /// Returns a reference to the L1 backend.
    pub fn l1(&self) -> &L1 {
        &self.l1
    }

    /// Returns a reference to the L2 backend.
    pub fn l2(&self) -> &L2 {
        &self.l2
    }

    /// Returns a snapshot of the current tier statistics.
    pub fn stats(&self) -> TierStats {
        self.tier_stats.snapshot()
    }

    /// Checks if an entry should be promoted from L2 to L1.
    fn should_promote(&self, key: &str) -> bool {
        let stats = self
            .key_stats
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(KeyStats::new()));

        match self.config.promotion_strategy {
            PromotionStrategy::HitCount { threshold } => stats.hits() >= threshold,
            PromotionStrategy::HitRate {
                threshold_per_minute,
            } => stats.hits_in_last_minute() >= threshold_per_minute,
        }
    }

    /// Records a hit on a key and returns the hit count.
    fn record_hit(&self, key: &str) -> u64 {
        self.key_stats
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(KeyStats::new()))
            .record_hit()
    }

    /// Promotes an entry from L2 to L1.
    #[allow(dead_code)]
    async fn promote(
        &self,
        key: &str,
        entry: CacheEntry,
        ttl: Duration,
        stale_for: Duration,
    ) -> Result<(), CacheError> {
        // Store in L1
        self.l1.set(key.to_string(), entry, ttl, stale_for).await?;

        // Reset promotion counter
        if let Some(stats) = self.key_stats.get(key) {
            stats.reset();
        }

        Ok(())
    }
}

impl<L1, L2> CacheBackend for MultiTierBackend<L1, L2>
where
    L1: CacheBackend,
    L2: CacheBackend,
{
    async fn get(&self, key: &str) -> Result<Option<CacheRead>, CacheError> {
        // Try L1 first
        if let Some(entry) = self.l1.get(key).await? {
            self.tier_stats.l1_hits.fetch_add(1, Ordering::Relaxed);
            #[cfg(feature = "metrics")]
            metrics::counter!("tower_http_cache.tier.l1_hit").increment(1);
            return Ok(Some(entry));
        }

        // Try L2
        if let Some(read) = self.l2.get(key).await? {
            self.tier_stats.l2_hits.fetch_add(1, Ordering::Relaxed);
            #[cfg(feature = "metrics")]
            metrics::counter!("tower_http_cache.tier.l2_hit").increment(1);

            // Record hit and check for promotion
            self.record_hit(key);

            if self.should_promote(key) {
                let entry_size = read.entry.body.len();

                // Check if entry is small enough for L1
                let should_promote_l1 = if let Some(max_size) = self.config.max_l1_entry_size {
                    entry_size <= max_size
                } else {
                    true
                };

                if should_promote_l1 {
                    self.tier_stats.promotions.fetch_add(1, Ordering::Relaxed);
                    #[cfg(feature = "metrics")]
                    metrics::counter!("tower_http_cache.tier.promoted").increment(1);

                    // Calculate remaining TTL for promotion
                    let ttl = if let Some(expires_at) = read.expires_at {
                        expires_at
                            .duration_since(std::time::SystemTime::now())
                            .unwrap_or(Duration::from_secs(60))
                    } else {
                        Duration::from_secs(60)
                    };

                    let stale_for = if let (Some(stale_until), Some(expires_at)) =
                        (read.stale_until, read.expires_at)
                    {
                        stale_until.duration_since(expires_at).unwrap_or_default()
                    } else {
                        Duration::ZERO
                    };

                    // Promote asynchronously (best effort)
                    let entry = read.entry.clone();
                    let key = key.to_string();
                    let l1 = self.l1.clone();
                    let key_stats = self.key_stats.clone();

                    tokio::spawn(async move {
                        let _ = l1.set(key.clone(), entry, ttl, stale_for).await;
                        if let Some(stats) = key_stats.get(&key) {
                            stats.reset();
                        }
                    });
                } else {
                    #[cfg(feature = "metrics")]
                    metrics::counter!("tower_http_cache.tier.promotion_skipped_large").increment(1);

                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        key = %key,
                        size = entry_size,
                        max_l1_size = ?self.config.max_l1_entry_size,
                        "skipping promotion for large entry"
                    );
                }
            }

            return Ok(Some(read));
        }

        self.tier_stats.misses.fetch_add(1, Ordering::Relaxed);
        Ok(None)
    }

    async fn set(
        &self,
        key: String,
        entry: CacheEntry,
        ttl: Duration,
        stale_for: Duration,
    ) -> Result<(), CacheError> {
        let entry_size = entry.body.len();

        // Always write to L2
        self.l2
            .set(key.clone(), entry.clone(), ttl, stale_for)
            .await?;
        self.tier_stats.writes.fetch_add(1, Ordering::Relaxed);

        // Optionally write to L1 if write-through is enabled and size is acceptable
        if self.config.write_through {
            // Written as a mutable flag rather than an if/else returning bool
            // literals: with `metrics` and `tracing` both off, the cfg'd
            // statements vanish and the else branch collapses to a bare
            // `false`, which trips `clippy::needless_bool`.
            let mut should_write_l1 = true;
            if let Some(max_size) = self.config.max_l1_entry_size {
                if entry_size > max_size {
                    #[cfg(feature = "metrics")]
                    metrics::counter!("tower_http_cache.tier.l1_skipped_large").increment(1);

                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        key = %key,
                        size = entry_size,
                        max_l1_size = max_size,
                        "skipping L1 write for large entry"
                    );

                    should_write_l1 = false;
                }
            }

            if should_write_l1 {
                let _ = self.l1.set(key.clone(), entry, ttl, stale_for).await;
            }
        }

        Ok(())
    }

    async fn invalidate(&self, key: &str) -> Result<(), CacheError> {
        // Invalidate both tiers
        let l1_result = self.l1.invalidate(key).await;
        let l2_result = self.l2.invalidate(key).await;

        // Remove stats
        self.key_stats.remove(key);

        // Return first error if any
        l1_result.and(l2_result)
    }

    /// Merges both tiers.
    ///
    /// A tier that reports [`CacheError::Unsupported`] — the shared backends
    /// keep no tag index — contributes nothing rather than failing the whole
    /// call, so the common `InMemoryBackend` over `RedisBackend` arrangement
    /// keeps working off the L1 index. Only if *neither* tier has an index is
    /// the `Unsupported` error propagated.
    async fn get_keys_by_tag(&self, tag: &str) -> Result<Vec<String>, CacheError> {
        let l1 = self.l1.get_keys_by_tag(tag).await;
        let l2 = self.l2.get_keys_by_tag(tag).await;

        let (mut keys, l2_keys) = match merge_tiers(l1, l2)? {
            Some(pair) => pair,
            None => return Ok(Vec::new()),
        };

        // Deduplicate
        keys.extend(l2_keys);
        keys.sort();
        keys.dedup();

        Ok(keys)
    }

    /// Invalidates in both tiers, tolerating a tier without a tag index in the
    /// same way as [`get_keys_by_tag`](Self::get_keys_by_tag).
    async fn invalidate_by_tag(&self, tag: &str) -> Result<usize, CacheError> {
        let l1 = self.l1.invalidate_by_tag(tag).await;
        let l2 = self.l2.invalidate_by_tag(tag).await;

        match merge_tiers(l1, l2)? {
            Some((l1_count, l2_count)) => Ok(l1_count + l2_count),
            None => Ok(0),
        }
    }

    /// Merges both tiers, tolerating a tier without a tag index in the same
    /// way as [`get_keys_by_tag`](Self::get_keys_by_tag).
    async fn list_tags(&self) -> Result<Vec<String>, CacheError> {
        let l1 = self.l1.list_tags().await;
        let l2 = self.l2.list_tags().await;

        let (mut tags, l2_tags) = match merge_tiers(l1, l2)? {
            Some(pair) => pair,
            None => return Ok(Vec::new()),
        };

        tags.extend(l2_tags);
        tags.sort();
        tags.dedup();

        Ok(tags)
    }
}

/// Combines a tag-index result from each tier.
///
/// `Ok(Some((l1, l2)))` when at least one tier answered — an `Unsupported`
/// tier contributes `T::default()`. `Ok(None)` when *both* tiers reported
/// `Unsupported`, which the caller renders as an empty answer. Any other error
/// propagates unchanged.
fn merge_tiers<T: Default>(
    l1: Result<T, CacheError>,
    l2: Result<T, CacheError>,
) -> Result<Option<(T, T)>, CacheError> {
    match (l1, l2) {
        (Ok(a), Ok(b)) => Ok(Some((a, b))),
        (Ok(a), Err(e)) if e.is_unsupported() => Ok(Some((a, T::default()))),
        (Err(e), Ok(b)) if e.is_unsupported() => Ok(Some((T::default(), b))),
        (Err(a), Err(b)) if a.is_unsupported() && b.is_unsupported() => Ok(None),
        (Err(e), _) | (_, Err(e)) => Err(e),
    }
}

/// Builder for configuring a multi-tier backend.
pub struct MultiTierBuilder<L1, L2> {
    l1: Option<L1>,
    l2: Option<L2>,
    config: MultiTierConfig,
}

impl<L1, L2> MultiTierBuilder<L1, L2> {
    /// Creates a new builder.
    pub fn new() -> Self {
        Self {
            l1: None,
            l2: None,
            config: MultiTierConfig::default(),
        }
    }

    /// Sets the L1 (fast) cache backend.
    pub fn l1(mut self, backend: L1) -> Self {
        self.l1 = Some(backend);
        self
    }

    /// Sets the L2 (persistent) cache backend.
    pub fn l2(mut self, backend: L2) -> Self {
        self.l2 = Some(backend);
        self
    }

    /// Sets the promotion strategy.
    pub fn promotion_strategy(mut self, strategy: PromotionStrategy) -> Self {
        self.config.promotion_strategy = strategy;
        self
    }

    /// Sets the promotion threshold for hit-count based promotion.
    pub fn promotion_threshold(mut self, threshold: u64) -> Self {
        self.config.promotion_strategy = PromotionStrategy::HitCount { threshold };
        self
    }

    /// Enables or disables write-through to L1.
    pub fn write_through(mut self, enabled: bool) -> Self {
        self.config.write_through = enabled;
        self
    }

    /// Sets the maximum entry size for L1 cache.
    /// Entries larger than this will only be stored in L2.
    pub fn max_l1_entry_size(mut self, size: Option<usize>) -> Self {
        self.config.max_l1_entry_size = size;
        self
    }

    /// Builds the multi-tier backend.
    pub fn build(self) -> MultiTierBackend<L1, L2> {
        MultiTierBackend {
            l1: self.l1.expect("L1 backend is required"),
            l2: self.l2.expect("L2 backend is required"),
            config: self.config,
            key_stats: Arc::new(DashMap::new()),
            tier_stats: Arc::new(AtomicTierStats::default()),
        }
    }
}

impl<L1, L2> Default for MultiTierBuilder<L1, L2> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, feature = "in-memory"))]
mod tests {
    use super::*;
    use crate::backend::memory::InMemoryBackend;
    use bytes::Bytes;
    use http::{StatusCode, Version};

    fn test_entry() -> CacheEntry {
        CacheEntry::new(
            StatusCode::OK,
            Version::HTTP_11,
            Vec::new(),
            Bytes::from_static(b"test"),
        )
    }

    #[tokio::test]
    async fn multi_tier_l1_hit() {
        let l1 = InMemoryBackend::new(100);
        let l2 = InMemoryBackend::new(1000);
        let backend = MultiTierBackend::new(l1.clone(), l2);

        // Store in L1
        l1.set(
            "key".to_string(),
            test_entry(),
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();

        // Should hit L1
        let result = backend.get("key").await.unwrap();
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn multi_tier_l2_hit_and_promote() {
        let l1 = InMemoryBackend::new(100);
        let l2 = InMemoryBackend::new(1000);

        let backend = MultiTierBackend::builder()
            .l1(l1.clone())
            .l2(l2.clone())
            .promotion_threshold(3)
            .build();

        // Store in L2 only
        l2.set(
            "key".to_string(),
            test_entry(),
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();

        // First few hits should be from L2
        for _ in 0..3 {
            let result = backend.get("key").await.unwrap();
            assert!(result.is_some());
        }

        // Give promotion task time to complete
        tokio::time::sleep(Duration::from_millis(50)).await;

        // After promotion threshold, should be in L1
        let l1_result = l1.get("key").await.unwrap();
        assert!(l1_result.is_some());
    }

    #[tokio::test]
    async fn multi_tier_set_writes_to_both_tiers() {
        let l1 = InMemoryBackend::new(100);
        let l2 = InMemoryBackend::new(1000);
        let backend = MultiTierBackend::builder()
            .l1(l1.clone())
            .l2(l2.clone())
            .write_through(true)
            .build();

        backend
            .set(
                "key".to_string(),
                test_entry(),
                Duration::from_secs(60),
                Duration::ZERO,
            )
            .await
            .unwrap();

        // Should be in both L1 and L2
        assert!(l1.get("key").await.unwrap().is_some());
        assert!(l2.get("key").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn multi_tier_invalidate_both_tiers() {
        let l1 = InMemoryBackend::new(100);
        let l2 = InMemoryBackend::new(1000);
        let backend = MultiTierBackend::new(l1.clone(), l2.clone());

        // Store in both
        l1.set(
            "key".to_string(),
            test_entry(),
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();
        l2.set(
            "key".to_string(),
            test_entry(),
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();

        // Invalidate through multi-tier
        backend.invalidate("key").await.unwrap();

        // Should be removed from both
        assert!(l1.get("key").await.unwrap().is_none());
        assert!(l2.get("key").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn multi_tier_miss() {
        let l1 = InMemoryBackend::new(100);
        let l2 = InMemoryBackend::new(1000);
        let backend = MultiTierBackend::new(l1, l2);

        let result = backend.get("nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn promotion_strategy_hit_count() {
        let strategy = PromotionStrategy::HitCount { threshold: 5 };
        let l1 = InMemoryBackend::new(100);
        let l2 = InMemoryBackend::new(1000);

        let backend = MultiTierBackend::builder()
            .l1(l1.clone())
            .l2(l2.clone())
            .promotion_strategy(strategy)
            .build();

        l2.set(
            "key".to_string(),
            test_entry(),
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();

        // Hit 5 times to trigger promotion
        for _ in 0..5 {
            backend.get("key").await.unwrap();
        }

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Should be promoted to L1
        assert!(l1.get("key").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn tier_stats_tracks_hits_misses_writes_and_promotions() {
        let l1 = InMemoryBackend::new(100);
        let l2 = InMemoryBackend::new(1000);

        let backend = MultiTierBackend::builder()
            .l1(l1.clone())
            .l2(l2.clone())
            .write_through(false)
            .promotion_threshold(2)
            .build();

        // Write goes only to L2 when write_through = false.
        backend
            .set(
                "hot".to_string(),
                test_entry(),
                Duration::from_secs(60),
                Duration::ZERO,
            )
            .await
            .unwrap();

        // 1 miss
        assert!(backend.get("missing").await.unwrap().is_none());

        // 2 L2 hits -> triggers promotion
        assert!(backend.get("hot").await.unwrap().is_some());
        assert!(backend.get("hot").await.unwrap().is_some());

        tokio::time::sleep(Duration::from_millis(50)).await;

        // 1 L1 hit
        assert!(backend.get("hot").await.unwrap().is_some());

        let stats = backend.stats();
        assert_eq!(stats.writes, 1);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.l2_hits, 2);
        assert_eq!(stats.promotions, 1);
        assert_eq!(stats.l1_hits, 1);
    }

    #[tokio::test]
    async fn promotion_strategy_hit_rate_enforces_configured_threshold() {
        let l1 = InMemoryBackend::new(100);
        let l2 = InMemoryBackend::new(1000);

        let backend = MultiTierBackend::builder()
            .l1(l1.clone())
            .l2(l2.clone())
            .promotion_strategy(PromotionStrategy::HitRate {
                threshold_per_minute: 5,
            })
            .build();

        l2.set(
            "rate-key".to_string(),
            test_entry(),
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();

        // 4 hits within 60s must NOT promote yet (threshold is 5/min).
        for _ in 0..4 {
            assert!(backend.get("rate-key").await.unwrap().is_some());
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(l1.get("rate-key").await.unwrap().is_none());

        // 5th hit within 60s triggers promotion.
        assert!(backend.get("rate-key").await.unwrap().is_some());
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(l1.get("rate-key").await.unwrap().is_some());
    }
}

#[cfg(test)]
mod merge_tier_tests {
    use super::merge_tiers;
    use crate::error::CacheError;

    fn unsupported<T>() -> Result<T, CacheError> {
        Err(CacheError::Unsupported("no tag index".to_string()))
    }

    #[test]
    fn both_tiers_answer() {
        let merged = merge_tiers(Ok(vec!["a".to_string()]), Ok(vec!["b".to_string()])).unwrap();
        assert_eq!(merged, Some((vec!["a".to_string()], vec!["b".to_string()])));
    }

    /// The common arrangement: an in-memory L1 with a tag index over a shared
    /// L2 without one. The L1 index must keep working.
    #[test]
    fn an_unsupported_tier_contributes_nothing_instead_of_failing() {
        let merged = merge_tiers(Ok(vec!["a".to_string()]), unsupported()).unwrap();
        assert_eq!(merged, Some((vec!["a".to_string()], Vec::new())));

        let merged = merge_tiers(unsupported(), Ok(vec!["b".to_string()])).unwrap();
        assert_eq!(merged, Some((Vec::new(), vec!["b".to_string()])));
    }

    #[test]
    fn neither_tier_has_an_index() {
        assert_eq!(
            merge_tiers::<Vec<String>>(unsupported(), unsupported()).unwrap(),
            None
        );
    }

    #[test]
    fn a_real_error_still_propagates() {
        let err = merge_tiers::<Vec<String>>(
            Ok(Vec::new()),
            Err(CacheError::Backend("connection reset".to_string())),
        )
        .unwrap_err();
        assert!(!err.is_unsupported());
    }
}
