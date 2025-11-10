//! ML-ready structured logging for cache operations.
//!
//! This module provides comprehensive structured logging suitable for
//! machine learning training and analysis. All cache operations emit
//! JSON-formatted logs with rich metadata for correlation and analysis.

use crate::request_id::RequestId;
use http::{Method, StatusCode, Uri, Version};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime};

/// Configuration for ML-ready structured logging.
#[derive(Debug, Clone)]
pub struct MLLoggingConfig {
    /// Enable ML-ready structured logging
    pub enabled: bool,

    /// Sample rate (1.0 = all requests, 0.1 = 10%)
    pub sample_rate: f64,

    /// Hash cache keys for privacy (recommended for production)
    pub hash_keys: bool,

    /// Target for structured logs (defaults to "tower_http_cache::ml")
    pub target: String,
}

impl Default for MLLoggingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sample_rate: 1.0,
            hash_keys: true,
            target: "tower_http_cache::ml".to_string(),
        }
    }
}

impl MLLoggingConfig {
    /// Creates a new ML logging configuration with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enables ML logging.
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Sets the sample rate (0.0 to 1.0).
    pub fn with_sample_rate(mut self, rate: f64) -> Self {
        self.sample_rate = rate.clamp(0.0, 1.0);
        self
    }

    /// Enables or disables key hashing.
    pub fn with_hash_keys(mut self, hash: bool) -> Self {
        self.hash_keys = hash;
        self
    }

    /// Sets a custom logging target.
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = target.into();
        self
    }

    /// Checks if this request should be logged based on sampling rate.
    pub fn should_sample(&self) -> bool {
        if !self.enabled {
            return false;
        }
        if self.sample_rate >= 1.0 {
            return true;
        }
        use std::collections::hash_map::RandomState;
        use std::hash::BuildHasher;
        let hasher = RandomState::new();

        let random = (hasher.hash_one(std::time::SystemTime::now()) as f64) / (u64::MAX as f64);
        random < self.sample_rate
    }
}

/// Types of cache events that can be logged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheEventType {
    /// Cache lookup hit (fresh entry)
    Hit,
    /// Cache lookup miss
    Miss,
    /// Stale entry served
    StaleHit,
    /// Cache entry stored
    Store,
    /// Cache entry invalidated
    Invalidate,
    /// Tag-based invalidation
    TagInvalidate,
    /// Multi-tier cache promotion
    TierPromote,
    /// Admin API access
    AdminAccess,
}

/// Structured cache event for ML training.
#[derive(Debug, Clone)]
pub struct CacheEvent {
    /// Timestamp of the event
    pub timestamp: SystemTime,

    /// Type of cache event
    pub event_type: CacheEventType,

    /// Request ID for correlation
    pub request_id: RequestId,

    /// Cache key (may be hashed)
    pub key: String,

    /// Request method
    pub method: Option<Method>,

    /// Request URI
    pub uri: Option<Uri>,

    /// HTTP version
    pub version: Option<Version>,

    /// Response status code
    pub status: Option<StatusCode>,

    /// Whether this was a cache hit
    pub hit: bool,

    /// Operation latency in microseconds
    pub latency_us: Option<u64>,

    /// Response size in bytes
    pub size_bytes: Option<usize>,

    /// TTL in seconds
    pub ttl_seconds: Option<u64>,

    /// Cache tags associated with this entry
    pub tags: Option<Vec<String>>,

    /// Tier information (l1, l2, or None)
    pub tier: Option<String>,

    /// Whether entry was promoted between tiers
    pub promoted: bool,

    /// Additional metadata
    pub metadata: serde_json::Value,
}

impl CacheEvent {
    /// Creates a new cache event.
    pub fn new(event_type: CacheEventType, request_id: RequestId, key: String) -> Self {
        Self {
            timestamp: SystemTime::now(),
            event_type,
            request_id,
            key,
            method: None,
            uri: None,
            version: None,
            status: None,
            hit: false,
            latency_us: None,
            size_bytes: None,
            ttl_seconds: None,
            tags: None,
            tier: None,
            promoted: false,
            metadata: json!({}),
        }
    }

    /// Sets the HTTP method.
    pub fn with_method(mut self, method: Method) -> Self {
        self.method = Some(method);
        self
    }

    /// Sets the request URI.
    pub fn with_uri(mut self, uri: Uri) -> Self {
        self.uri = Some(uri);
        self
    }

    /// Sets the HTTP version.
    pub fn with_version(mut self, version: Version) -> Self {
        self.version = Some(version);
        self
    }

    /// Sets the response status.
    pub fn with_status(mut self, status: StatusCode) -> Self {
        self.status = Some(status);
        self
    }

    /// Sets whether this was a cache hit.
    pub fn with_hit(mut self, hit: bool) -> Self {
        self.hit = hit;
        self
    }

    /// Sets the operation latency.
    pub fn with_latency(mut self, latency: Duration) -> Self {
        self.latency_us = Some(latency.as_micros() as u64);
        self
    }

    /// Sets the response size.
    pub fn with_size(mut self, size: usize) -> Self {
        self.size_bytes = Some(size);
        self
    }

    /// Sets the TTL.
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl_seconds = Some(ttl.as_secs());
        self
    }

    /// Sets the cache tags.
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = Some(tags);
        self
    }

    /// Sets the tier information.
    pub fn with_tier(mut self, tier: impl Into<String>) -> Self {
        self.tier = Some(tier.into());
        self
    }

    /// Sets whether the entry was promoted.
    pub fn with_promoted(mut self, promoted: bool) -> Self {
        self.promoted = promoted;
        self
    }

    /// Adds custom metadata.
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }

    /// Logs this event using the provided configuration.
    pub fn log(&self, config: &MLLoggingConfig) {
        if !config.should_sample() {
            return;
        }

        let key = if config.hash_keys {
            hash_key(&self.key)
        } else {
            self.key.clone()
        };

        let timestamp = self
            .timestamp
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();

        let log_data = json!({
            "timestamp": format!("{}.{:03}Z",
                chrono::DateTime::<chrono::Utc>::from(self.timestamp)
                    .format("%Y-%m-%dT%H:%M:%S"),
                timestamp.subsec_millis()
            ),
            "level": "info",
            "event": format!("{:?}", self.event_type).to_lowercase(),
            "request_id": self.request_id.as_str(),
            "key": key,
            "method": self.method.as_ref().map(|m| m.as_str()),
            "uri": self.uri.as_ref().map(|u| u.to_string()),
            "version": self.version.as_ref().map(|v| format!("{:?}", v)),
            "status": self.status.as_ref().map(|s| s.as_u16()),
            "hit": self.hit,
            "latency_us": self.latency_us,
            "size_bytes": self.size_bytes,
            "ttl_seconds": self.ttl_seconds,
            "tags": self.tags,
            "tier": self.tier,
            "promoted": self.promoted,
            "metadata": self.metadata,
        });

        #[cfg(feature = "tracing")]
        {
            // Use fixed target for tracing, but include the configured target in the log data
            tracing::info!(
                target: "tower_http_cache::ml",
                event = %log_data
            );
        }

        #[cfg(not(feature = "tracing"))]
        {
            // Fallback to println for non-tracing builds
            let _ = config; // suppress warning
            println!("{}", log_data);
        }
    }
}

/// Hashes a cache key using SHA-256 for privacy.
pub fn hash_key(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}

/// Helper to log a simple cache operation.
pub fn log_cache_operation(
    config: &MLLoggingConfig,
    event_type: CacheEventType,
    request_id: RequestId,
    key: String,
) {
    if !config.enabled {
        return;
    }

    let event = CacheEvent::new(event_type, request_id, key);
    event.log(config);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ml_logging_config_default() {
        let config = MLLoggingConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.sample_rate, 1.0);
        assert!(config.hash_keys);
    }

    #[test]
    fn ml_logging_config_builder() {
        let config = MLLoggingConfig::new()
            .with_enabled(true)
            .with_sample_rate(0.5)
            .with_hash_keys(false)
            .with_target("custom::target");

        assert!(config.enabled);
        assert_eq!(config.sample_rate, 0.5);
        assert!(!config.hash_keys);
        assert_eq!(config.target, "custom::target");
    }

    #[test]
    fn sample_rate_clamped() {
        let config = MLLoggingConfig::new().with_sample_rate(1.5);
        assert_eq!(config.sample_rate, 1.0);

        let config = MLLoggingConfig::new().with_sample_rate(-0.5);
        assert_eq!(config.sample_rate, 0.0);
    }

    #[test]
    fn should_sample_when_disabled() {
        let config = MLLoggingConfig::new().with_enabled(false);
        assert!(!config.should_sample());
    }

    #[test]
    fn should_sample_when_rate_is_one() {
        let config = MLLoggingConfig::new()
            .with_enabled(true)
            .with_sample_rate(1.0);
        assert!(config.should_sample());
    }

    #[test]
    fn hash_key_consistent() {
        let key = "/api/users/123";
        let hash1 = hash_key(key);
        let hash2 = hash_key(key);
        assert_eq!(hash1, hash2);
        assert_ne!(hash1, key);
        assert_eq!(hash1.len(), 64); // SHA-256 produces 64 hex chars
    }

    #[test]
    fn cache_event_builder() {
        let request_id = RequestId::new();
        let event = CacheEvent::new(CacheEventType::Hit, request_id.clone(), "/test".to_string())
            .with_method(Method::GET)
            .with_status(StatusCode::OK)
            .with_hit(true)
            .with_latency(Duration::from_micros(150))
            .with_size(1024)
            .with_ttl(Duration::from_secs(300))
            .with_tags(vec!["user:123".to_string()])
            .with_tier("l1")
            .with_promoted(false);

        assert_eq!(event.method, Some(Method::GET));
        assert_eq!(event.status, Some(StatusCode::OK));
        assert!(event.hit);
        assert_eq!(event.latency_us, Some(150));
        assert_eq!(event.size_bytes, Some(1024));
        assert_eq!(event.ttl_seconds, Some(300));
        assert_eq!(event.tags, Some(vec!["user:123".to_string()]));
        assert_eq!(event.tier, Some("l1".to_string()));
        assert!(!event.promoted);
    }

    #[test]
    fn cache_event_log_disabled() {
        let config = MLLoggingConfig::new().with_enabled(false);
        let request_id = RequestId::new();
        let event = CacheEvent::new(CacheEventType::Hit, request_id, "/test".to_string());

        // Should not panic when logging is disabled
        event.log(&config);
    }

    #[test]
    fn cache_event_log_with_hashing() {
        let config = MLLoggingConfig::new()
            .with_enabled(true)
            .with_hash_keys(true);
        let request_id = RequestId::new();
        let event = CacheEvent::new(CacheEventType::Hit, request_id, "/api/secret".to_string());

        // Should not panic when logging with hashing
        event.log(&config);
    }

    #[test]
    fn log_cache_operation_helper() {
        let config = MLLoggingConfig::new().with_enabled(true);
        let request_id = RequestId::new();

        // Should not panic
        log_cache_operation(
            &config,
            CacheEventType::Miss,
            request_id,
            "/test".to_string(),
        );
    }
}
