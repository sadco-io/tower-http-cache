//! Admin API route handlers.
//!
//! This module provides the HTTP endpoint handlers for the admin API.
//! Note: Full Axum integration would be feature-gated. This provides
//! the core logic that can be wrapped by any HTTP framework.

use crate::admin::{AdminConfig, AdminState};
use crate::backend::CacheBackend;
use serde::{Deserialize, Serialize};

/// Health check response.
#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub timestamp: String,
}

/// Statistics response.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatsResponse {
    pub cache: CacheStats,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CacheStats {
    pub backend: String,
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

/// Hot keys response.
#[derive(Debug, Serialize, Deserialize)]
pub struct HotKeysResponse {
    pub hot_keys: Vec<HotKeyEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HotKeyEntry {
    pub key: String,
    pub hits: u64,
    pub last_accessed: String,
}

/// Tags list response.
#[derive(Debug, Serialize, Deserialize)]
pub struct TagsResponse {
    pub tags: Vec<String>,
    pub count: usize,
}

/// Invalidation request.
#[derive(Debug, Serialize, Deserialize)]
pub struct InvalidationRequest {
    pub key: Option<String>,
    pub tag: Option<String>,
    pub tags: Option<Vec<String>>,
}

/// Invalidation response.
#[derive(Debug, Serialize, Deserialize)]
pub struct InvalidationResponse {
    pub success: bool,
    pub keys_invalidated: usize,
    pub message: String,
}

/// Error response.
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
    pub status: u16,
}

/// Handles health check requests.
pub fn handle_health() -> HealthResponse {
    HealthResponse {
        status: "healthy".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    }
}

/// Handles statistics requests.
pub fn handle_stats<B: CacheBackend>(state: &AdminState<B>) -> StatsResponse {
    let snapshot = state.stats.snapshot();

    StatsResponse {
        cache: CacheStats {
            backend: "cache".to_string(),
            total_requests: snapshot.total_requests,
            hits: snapshot.hits,
            misses: snapshot.misses,
            stale_hits: snapshot.stale_hits,
            stores: snapshot.stores,
            invalidations: snapshot.invalidations,
            hit_rate: snapshot.hit_rate,
            miss_rate: snapshot.miss_rate,
            uptime_seconds: snapshot.uptime_seconds,
        },
    }
}

/// Handles hot keys requests.
pub fn handle_hot_keys<B: CacheBackend>(
    state: &AdminState<B>,
    limit: Option<usize>,
) -> HotKeysResponse {
    let limit = limit.unwrap_or(state.config.max_hot_keys).min(100);
    let hot_keys = state.stats.hot_keys(limit);

    HotKeysResponse {
        hot_keys: hot_keys
            .into_iter()
            .map(|k| HotKeyEntry {
                key: k.key,
                hits: k.hits,
                last_accessed: chrono::DateTime::<chrono::Utc>::from(k.last_accessed).to_rfc3339(),
            })
            .collect(),
    }
}

/// Handles tag listing requests.
pub async fn handle_list_tags<B: CacheBackend>(
    state: &AdminState<B>,
) -> Result<TagsResponse, String> {
    let tags = state
        .backend
        .list_tags()
        .await
        .map_err(|e| format!("Failed to list tags: {}", e))?;

    let count = tags.len();

    Ok(TagsResponse { tags, count })
}

/// Handles invalidation requests.
pub async fn handle_invalidate<B: CacheBackend>(
    state: &AdminState<B>,
    request: InvalidationRequest,
) -> Result<InvalidationResponse, String> {
    if let Some(key) = request.key {
        // Invalidate single key
        state
            .backend
            .invalidate(&key)
            .await
            .map_err(|e| format!("Failed to invalidate key: {}", e))?;

        state.stats.record_invalidation();

        Ok(InvalidationResponse {
            success: true,
            keys_invalidated: 1,
            message: format!("Invalidated key: {}", key),
        })
    } else if let Some(tag) = request.tag {
        // Invalidate by single tag
        let count = state
            .backend
            .invalidate_by_tag(&tag)
            .await
            .map_err(|e| format!("Failed to invalidate by tag: {}", e))?;

        for _ in 0..count {
            state.stats.record_invalidation();
        }

        Ok(InvalidationResponse {
            success: true,
            keys_invalidated: count,
            message: format!("Invalidated {} keys with tag: {}", count, tag),
        })
    } else if let Some(tags) = request.tags {
        // Invalidate by multiple tags
        let count = state
            .backend
            .invalidate_by_tags(&tags)
            .await
            .map_err(|e| format!("Failed to invalidate by tags: {}", e))?;

        for _ in 0..count {
            state.stats.record_invalidation();
        }

        Ok(InvalidationResponse {
            success: true,
            keys_invalidated: count,
            message: format!("Invalidated {} keys", count),
        })
    } else {
        Err("Must provide key, tag, or tags".to_string())
    }
}

/// Validates authentication for admin requests.
pub fn validate_auth(config: &AdminConfig, auth_header: Option<&str>) -> bool {
    let token = auth_header.and_then(|h| {
        // Extract token from "Bearer <token>"
        h.strip_prefix("Bearer ").or(Some(h))
    });

    config.validate_token(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::backend::memory::InMemoryBackend;

    fn test_state() -> AdminState<InMemoryBackend> {
        let backend = InMemoryBackend::new(100);
        let config = AdminConfig::new();
        AdminState::new(backend, config)
    }

    #[test]
    fn handle_health_returns_healthy() {
        let response = handle_health();
        assert_eq!(response.status, "healthy");
        assert!(!response.timestamp.is_empty());
    }

    #[test]
    fn handle_stats_returns_snapshot() {
        let state = test_state();
        state.stats.record_hit("key1");
        state.stats.record_miss("key2");

        let response = handle_stats(&state);
        assert_eq!(response.cache.total_requests, 2);
        assert_eq!(response.cache.hits, 1);
        assert_eq!(response.cache.misses, 1);
    }

    #[test]
    fn handle_hot_keys_returns_sorted_keys() {
        let state = test_state();

        for _ in 0..10 {
            state.stats.record_hit("hot");
        }
        for _ in 0..5 {
            state.stats.record_hit("warm");
        }
        state.stats.record_hit("cold");

        let response = handle_hot_keys(&state, Some(2));
        assert_eq!(response.hot_keys.len(), 2);
        assert_eq!(response.hot_keys[0].key, "hot");
        assert_eq!(response.hot_keys[0].hits, 10);
    }

    #[test]
    fn handle_hot_keys_respects_limit() {
        let state = test_state();

        for i in 0..50 {
            state.stats.record_hit(&format!("key{}", i));
        }

        let response = handle_hot_keys(&state, Some(10));
        assert_eq!(response.hot_keys.len(), 10);

        // Should cap at 100 even if requested higher
        let response = handle_hot_keys(&state, Some(200));
        assert!(response.hot_keys.len() <= 100);
    }

    #[tokio::test]
    async fn handle_list_tags_returns_tags() {
        let state = test_state();

        // No tags initially
        let response = handle_list_tags(&state).await.unwrap();
        assert_eq!(response.count, 0);
    }

    #[tokio::test]
    async fn handle_invalidate_by_key() {
        let state = test_state();

        let request = InvalidationRequest {
            key: Some("test_key".to_string()),
            tag: None,
            tags: None,
        };

        let response = handle_invalidate(&state, request).await.unwrap();
        assert!(response.success);
        assert_eq!(response.keys_invalidated, 1);
    }

    #[tokio::test]
    async fn handle_invalidate_by_tag() {
        let state = test_state();

        let request = InvalidationRequest {
            key: None,
            tag: Some("user:123".to_string()),
            tags: None,
        };

        let response = handle_invalidate(&state, request).await.unwrap();
        assert!(response.success);
    }

    #[tokio::test]
    async fn handle_invalidate_by_tags() {
        let state = test_state();

        let request = InvalidationRequest {
            key: None,
            tag: None,
            tags: Some(vec!["tag1".to_string(), "tag2".to_string()]),
        };

        let response = handle_invalidate(&state, request).await.unwrap();
        assert!(response.success);
    }

    #[tokio::test]
    async fn handle_invalidate_requires_parameter() {
        let state = test_state();

        let request = InvalidationRequest {
            key: None,
            tag: None,
            tags: None,
        };

        let result = handle_invalidate(&state, request).await;
        assert!(result.is_err());
    }

    #[test]
    fn validate_auth_with_bearer_token() {
        let config = AdminConfig::new()
            .with_auth_token("secret123")
            .with_require_auth(true);

        assert!(validate_auth(&config, Some("Bearer secret123")));
        assert!(validate_auth(&config, Some("secret123")));
        assert!(!validate_auth(&config, Some("Bearer wrong")));
        assert!(!validate_auth(&config, None));
    }

    #[test]
    fn validate_auth_no_auth_required() {
        let config = AdminConfig::new().with_require_auth(false);

        assert!(validate_auth(&config, None));
        assert!(validate_auth(&config, Some("anything")));
    }
}
