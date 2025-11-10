//! Multi-tier caching backend with automatic promotion.
//!
//! This module implements a two-tier caching architecture where frequently
//! accessed entries are automatically promoted from a slower L2 cache to
//! a faster L1 cache based on configurable promotion strategies.

use async_trait::async_trait;
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::{CacheBackend, CacheEntry, CacheRead};
use crate::error::CacheError;

/// Strategy for promoting entries from L2 to L1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Default)]
pub struct TierStats {
    pub l1_hits: u64,
    pub l2_hits: u64,
    pub misses: u64,
    pub promotions: u64,
}

/// Configuration for multi-tier caching.
#[derive(Debug, Clone)]
pub struct MultiTierConfig {
    /// Strategy for promoting entries from L2 to L1
    pub promotion_strategy: PromotionStrategy,

    /// Whether to write to both tiers on set (true) or L2 only (false)
    pub write_through: bool,
}

impl Default for MultiTierConfig {
    fn default() -> Self {
        Self {
            promotion_strategy: PromotionStrategy::default(),
            write_through: true,
        }
    }
}

/// Per-key statistics for promotion tracking.
struct KeyStats {
    l2_hits: AtomicU64,
}

impl KeyStats {
    fn new() -> Self {
        Self {
            l2_hits: AtomicU64::new(0),
        }
    }

    fn record_hit(&self) -> u64 {
        self.l2_hits.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn reset(&self) {
        self.l2_hits.store(0, Ordering::Relaxed);
    }

    fn hits(&self) -> u64 {
        self.l2_hits.load(Ordering::Relaxed)
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
    tier_stats: Arc<TierStats>,
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
            tier_stats: Arc::new(TierStats::default()),
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

    /// Returns a reference to the current tier statistics.
    pub fn stats(&self) -> &TierStats {
        &self.tier_stats
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
                threshold_per_minute: _,
            } => {
                // For simplicity, use hit count for now
                // A full implementation would track timestamps
                stats.hits() >= 3
            }
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

#[async_trait]
impl<L1, L2> CacheBackend for MultiTierBackend<L1, L2>
where
    L1: CacheBackend,
    L2: CacheBackend,
{
    async fn get(&self, key: &str) -> Result<Option<CacheRead>, CacheError> {
        // Try L1 first
        if let Some(entry) = self.l1.get(key).await? {
            #[cfg(feature = "metrics")]
            metrics::counter!("tower_http_cache.tier.l1_hit").increment(1);
            return Ok(Some(entry));
        }

        // Try L2
        if let Some(read) = self.l2.get(key).await? {
            #[cfg(feature = "metrics")]
            metrics::counter!("tower_http_cache.tier.l2_hit").increment(1);

            // Record hit and check for promotion
            self.record_hit(key);

            if self.should_promote(key) {
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
            }

            return Ok(Some(read));
        }

        Ok(None)
    }

    async fn set(
        &self,
        key: String,
        entry: CacheEntry,
        ttl: Duration,
        stale_for: Duration,
    ) -> Result<(), CacheError> {
        // Always write to L2
        self.l2
            .set(key.clone(), entry.clone(), ttl, stale_for)
            .await?;

        // Optionally write to L1 if write-through is enabled
        if self.config.write_through {
            let _ = self.l1.set(key.clone(), entry, ttl, stale_for).await;
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

    async fn get_keys_by_tag(&self, tag: &str) -> Result<Vec<String>, CacheError> {
        // Query both tiers and merge results
        let mut keys = self.l1.get_keys_by_tag(tag).await?;
        let l2_keys = self.l2.get_keys_by_tag(tag).await?;

        // Deduplicate
        keys.extend(l2_keys);
        keys.sort();
        keys.dedup();

        Ok(keys)
    }

    async fn invalidate_by_tag(&self, tag: &str) -> Result<usize, CacheError> {
        // Invalidate in both tiers
        let l1_count = self.l1.invalidate_by_tag(tag).await?;
        let l2_count = self.l2.invalidate_by_tag(tag).await?;

        Ok(l1_count + l2_count)
    }

    async fn list_tags(&self) -> Result<Vec<String>, CacheError> {
        // Merge tags from both tiers
        let mut tags = self.l1.list_tags().await?;
        let l2_tags = self.l2.list_tags().await?;

        tags.extend(l2_tags);
        tags.sort();
        tags.dedup();

        Ok(tags)
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

    /// Builds the multi-tier backend.
    pub fn build(self) -> MultiTierBackend<L1, L2> {
        MultiTierBackend {
            l1: self.l1.expect("L1 backend is required"),
            l2: self.l2.expect("L2 backend is required"),
            config: self.config,
            key_stats: Arc::new(DashMap::new()),
            tier_stats: Arc::new(TierStats::default()),
        }
    }
}

impl<L1, L2> Default for MultiTierBuilder<L1, L2> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
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
}
