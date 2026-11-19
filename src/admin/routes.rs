//! Admin API route handlers.
//!
//! This module provides framework-agnostic handler functions for the admin API
//! that can be wrapped by any HTTP framework (Axum, Actix, Warp, Hyper, etc.).
//!
//! # Authentication and Deployment
//!
//! Every handler that reads or mutates cache state ([`handle_stats`],
//! [`handle_hot_keys`], [`handle_list_tags`], and [`handle_invalidate`])
//! accepts the request's raw `Authorization` header value and calls
//! [`AdminState::authorize`] first. Requests are refused with
//! [`AdminError::Disabled`] (HTTP `404`) when [`AdminConfig::enabled`] is
//! `false`, or [`AdminError::Unauthorized`] (HTTP `401`) when authentication
//! is required and the `Authorization: Bearer <token>` header is missing,
//! malformed, or invalid. Return a bare `404` for [`AdminError::Disabled`] so
//! disabled endpoints are not advertised, and include `WWW-Authenticate: Bearer`
//! on `401` responses.
//!
//! Do not mount any admin routes — including [`handle_health`] — when
//! [`AdminConfig::enabled`] is `false`. When enabled, [`handle_health`] stays
//! open for health probes.
//!
//! Serve these routes only on an internal-only listener (for example loopback
//! or a private management network) rather than on a public application router.
//!
//! # Example
//!
//! ```no_run
//! use tower_http_cache::admin::routes::{
//!     ErrorResponse, InvalidationRequest, InvalidationResponse, handle_invalidate,
//! };
//! use tower_http_cache::admin::{AdminError, AdminState};
//! use tower_http_cache::backend::CacheBackend;
//!
//! async fn invalidate_endpoint<B: CacheBackend>(
//!     state: &AdminState<B>,
//!     auth_header: Option<&str>,
//!     body: InvalidationRequest,
//! ) -> Result<InvalidationResponse, (u16, Option<ErrorResponse>)> {
//!     handle_invalidate(state, auth_header, body)
//!         .await
//!         .map_err(|err| match err {
//!             AdminError::Disabled => (err.status(), None),
//!             other => (other.status(), Some(ErrorResponse::from(other))),
//!         })
//! }
//! ```

use crate::admin::{AdminConfig, AdminError, AdminState};
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

impl From<&AdminError> for ErrorResponse {
    fn from(err: &AdminError) -> Self {
        Self {
            error: err.to_string(),
            status: err.status(),
        }
    }
}

impl From<AdminError> for ErrorResponse {
    fn from(err: AdminError) -> Self {
        Self::from(&err)
    }
}

/// Handles health check requests.
///
/// This endpoint does not require authentication so liveness and readiness
/// probes can query it without credentials. Do not mount this route (or any
/// other admin route) when [`AdminConfig::enabled`] is `false`.
pub fn handle_health() -> HealthResponse {
    HealthResponse {
        status: "healthy".to_string(),
        timestamp: crate::time_fmt::format_rfc3339(std::time::SystemTime::now()),
    }
}

/// Handles cache statistics requests.
///
/// Authenticates the request via [`AdminState::authorize`] using the raw
/// `Authorization` header value in `auth_header` before reading statistics.
/// Returns [`AdminError::Disabled`] when the admin API is disabled or
/// [`AdminError::Unauthorized`] when authentication fails.
pub fn handle_stats<B: CacheBackend>(
    state: &AdminState<B>,
    auth_header: Option<&str>,
) -> Result<StatsResponse, AdminError> {
    state.authorize(auth_header)?;
    let snapshot = state.stats.snapshot();

    Ok(StatsResponse {
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
    })
}

/// Handles hot-keys inspection requests.
///
/// Authenticates the request via [`AdminState::authorize`] using the raw
/// `Authorization` header value in `auth_header` before returning tracked keys.
/// Returns [`AdminError::Disabled`] when the admin API is disabled or
/// [`AdminError::Unauthorized`] when authentication fails.
pub fn handle_hot_keys<B: CacheBackend>(
    state: &AdminState<B>,
    auth_header: Option<&str>,
    limit: Option<usize>,
) -> Result<HotKeysResponse, AdminError> {
    state.authorize(auth_header)?;
    let limit = limit.unwrap_or(state.config.max_hot_keys).min(100);
    let hot_keys = state.stats.hot_keys(limit);

    Ok(HotKeysResponse {
        hot_keys: hot_keys
            .into_iter()
            .map(|k| HotKeyEntry {
                key: k.key,
                hits: k.hits,
                last_accessed: crate::time_fmt::format_rfc3339(k.last_accessed),
            })
            .collect(),
    })
}

/// Handles cache tag listing requests.
///
/// Authenticates the request via [`AdminState::authorize`] using the raw
/// `Authorization` header value in `auth_header` before querying the backend.
/// Returns [`AdminError::Disabled`] when the admin API is disabled,
/// [`AdminError::Unauthorized`] when authentication fails,
/// [`AdminError::Unsupported`] when the backend does not support tag listing, or
/// [`AdminError::Backend`] if the backend returns an error.
pub async fn handle_list_tags<B: CacheBackend>(
    state: &AdminState<B>,
    auth_header: Option<&str>,
) -> Result<TagsResponse, AdminError> {
    state.authorize(auth_header)?;
    let tags = state
        .backend
        .list_tags()
        .await
        .map_err(|e| map_backend_error("Failed to list tags", e))?;

    let count = tags.len();

    Ok(TagsResponse { tags, count })
}

/// Handles cache invalidation requests.
///
/// Authenticates the request via [`AdminState::authorize`] using the raw
/// `Authorization` header value in `auth_header` before invalidating any keys
/// or tags. Returns [`AdminError::Disabled`] when the admin API is disabled,
/// [`AdminError::Unauthorized`] when authentication fails,
/// [`AdminError::BadRequest`] if the request input is missing or invalid,
/// [`AdminError::Unsupported`] when the backend does not support tag invalidation, or
/// [`AdminError::Backend`] if the backend returns an error.
pub async fn handle_invalidate<B: CacheBackend>(
    state: &AdminState<B>,
    auth_header: Option<&str>,
    request: InvalidationRequest,
) -> Result<InvalidationResponse, AdminError> {
    state.authorize(auth_header)?;
    invalidate_authorized(state, request).await
}

fn map_backend_error(context: &str, err: crate::error::CacheError) -> AdminError {
    let msg = format!("{context}: {err}");
    if err.is_unsupported() {
        AdminError::Unsupported(msg)
    } else {
        AdminError::Backend(msg)
    }
}

async fn invalidate_authorized<B: CacheBackend>(
    state: &AdminState<B>,
    request: InvalidationRequest,
) -> Result<InvalidationResponse, AdminError> {
    if let Some(key) = request.key {
        // Invalidate single key
        state
            .backend
            .invalidate(&key)
            .await
            .map_err(|e| map_backend_error("Failed to invalidate key", e))?;

        if let Some(chunk_cache) = &state.chunk_cache {
            chunk_cache.remove(&key);
        }

        state.stats.record_invalidation();

        Ok(InvalidationResponse {
            success: true,
            keys_invalidated: 1,
            message: format!("Invalidated key: {}", key),
        })
    } else if let Some(tag) = request.tag {
        // Capture tagged keys before invalidation so we can evict matching chunk entries too.
        let tagged_keys = if state.chunk_cache.is_some() {
            state.backend.get_keys_by_tag(&tag).await.ok()
        } else {
            None
        };

        // Invalidate by single tag
        let count = state
            .backend
            .invalidate_by_tag(&tag)
            .await
            .map_err(|e| map_backend_error("Failed to invalidate by tag", e))?;

        if let Some(chunk_cache) = &state.chunk_cache {
            evict_chunk_keys(chunk_cache, tagged_keys, count);
        }

        for _ in 0..count {
            state.stats.record_invalidation();
        }

        Ok(InvalidationResponse {
            success: true,
            keys_invalidated: count,
            message: format!("Invalidated {} keys with tag: {}", count, tag),
        })
    } else if let Some(tags) = request.tags {
        let tagged_keys = if state.chunk_cache.is_some() {
            let mut collected = Vec::new();
            let mut complete = true;
            for tag in &tags {
                match state.backend.get_keys_by_tag(tag).await {
                    Ok(mut keys) => collected.append(&mut keys),
                    Err(_) => {
                        complete = false;
                        break;
                    }
                }
            }
            if complete { Some(collected) } else { None }
        } else {
            None
        };

        // Invalidate by multiple tags
        let count = state
            .backend
            .invalidate_by_tags(&tags)
            .await
            .map_err(|e| map_backend_error("Failed to invalidate by tags", e))?;

        if let Some(chunk_cache) = &state.chunk_cache {
            evict_chunk_keys(chunk_cache, tagged_keys, count);
        }

        for _ in 0..count {
            state.stats.record_invalidation();
        }

        Ok(InvalidationResponse {
            success: true,
            keys_invalidated: count,
            message: format!("Invalidated {} keys", count),
        })
    } else {
        Err(AdminError::BadRequest(
            "Must provide key, tag, or tags".to_string(),
        ))
    }
}

fn evict_chunk_keys(
    chunk_cache: &crate::chunks::ChunkCache,
    keys: Option<Vec<String>>,
    invalidated_count: usize,
) {
    match keys {
        Some(keys) if !keys.is_empty() => {
            for key in keys {
                chunk_cache.remove(&key);
            }
        }
        _ if invalidated_count > 0 => {
            chunk_cache.clear();
        }
        _ => {}
    }
}

/// Validates authentication for admin requests.
pub fn validate_auth(config: &AdminConfig, auth_header: Option<&str>) -> bool {
    let token = auth_header.and_then(|h| {
        let trimmed = h.trim();
        let (scheme, rest) = trimmed.split_once(' ')?;
        if scheme.eq_ignore_ascii_case("bearer") {
            let token = rest.trim_start();
            if token.is_empty() { None } else { Some(token) }
        } else {
            None
        }
    });

    config.validate_token(token)
}

#[cfg(all(test, feature = "in-memory"))]
mod tests {
    use super::*;

    use crate::backend::CacheEntry;
    use crate::backend::memory::InMemoryBackend;
    use bytes::Bytes;
    use http::{StatusCode, Version};
    use std::time::Duration;

    const TOKEN: &str = "s3cret";
    const AUTH: Option<&str> = Some("Bearer s3cret");

    fn test_state() -> AdminState<InMemoryBackend> {
        let backend = InMemoryBackend::new(100);
        let config = AdminConfig::new().with_enabled(true).with_auth_token(TOKEN);
        AdminState::new(backend, config)
    }

    #[tokio::test]
    async fn handlers_refuse_missing_or_wrong_token() {
        let state = test_state();
        state
            .backend
            .set(
                "k".to_string(),
                CacheEntry::new(
                    StatusCode::OK,
                    Version::HTTP_11,
                    vec![],
                    Bytes::from_static(b"cached"),
                ),
                Duration::from_secs(60),
                Duration::ZERO,
            )
            .await
            .unwrap();

        for bad in [
            None,
            Some("Bearer wrong"),
            Some("s3cret"),
            Some("Basic s3cret"),
            Some("Bearer "),
        ] {
            assert_eq!(
                handle_stats(&state, bad).unwrap_err(),
                AdminError::Unauthorized
            );
            assert_eq!(
                handle_hot_keys(&state, bad, None).unwrap_err(),
                AdminError::Unauthorized
            );
            assert_eq!(
                handle_list_tags(&state, bad).await.unwrap_err(),
                AdminError::Unauthorized
            );
            let req = InvalidationRequest {
                key: Some("k".into()),
                tag: None,
                tags: None,
            };
            assert_eq!(
                handle_invalidate(&state, bad, req).await.unwrap_err(),
                AdminError::Unauthorized
            );
        }

        assert_eq!(state.stats.snapshot().invalidations, 0);
        assert!(state.backend.get("k").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn handlers_refuse_when_disabled() {
        let configs = [
            // enabled defaults to false with auth required and token configured
            AdminConfig::new().with_auth_token(TOKEN),
            // enabled is false even when auth is not required
            AdminConfig::new().with_require_auth(false),
        ];

        for config in configs {
            let state = AdminState::new(InMemoryBackend::new(10), config);
            state
                .backend
                .set(
                    "k".to_string(),
                    CacheEntry::new(
                        StatusCode::OK,
                        Version::HTTP_11,
                        vec![],
                        Bytes::from_static(b"cached"),
                    ),
                    Duration::from_secs(60),
                    Duration::ZERO,
                )
                .await
                .unwrap();

            for header in [AUTH, None] {
                assert_eq!(
                    handle_stats(&state, header).unwrap_err(),
                    AdminError::Disabled
                );
                assert_eq!(
                    handle_hot_keys(&state, header, None).unwrap_err(),
                    AdminError::Disabled
                );
                assert_eq!(
                    handle_list_tags(&state, header).await.unwrap_err(),
                    AdminError::Disabled
                );
                let req = InvalidationRequest {
                    key: Some("k".into()),
                    tag: None,
                    tags: None,
                };
                assert_eq!(
                    handle_invalidate(&state, header, req).await.unwrap_err(),
                    AdminError::Disabled
                );
            }

            assert_eq!(state.stats.snapshot().invalidations, 0);
            assert!(state.backend.get("k").await.unwrap().is_some());
        }
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

        let response = handle_stats(&state, AUTH).unwrap();
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

        let response = handle_hot_keys(&state, AUTH, Some(2)).unwrap();
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

        let response = handle_hot_keys(&state, AUTH, Some(10)).unwrap();
        assert_eq!(response.hot_keys.len(), 10);

        // Should cap at 100 even if requested higher
        let response = handle_hot_keys(&state, AUTH, Some(200)).unwrap();
        assert!(response.hot_keys.len() <= 100);
    }

    #[tokio::test]
    async fn handle_list_tags_returns_tags() {
        let state = test_state();

        // No tags initially
        let response = handle_list_tags(&state, AUTH).await.unwrap();
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

        let response = handle_invalidate(&state, AUTH, request).await.unwrap();
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

        let response = handle_invalidate(&state, AUTH, request).await.unwrap();
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

        let response = handle_invalidate(&state, AUTH, request).await.unwrap();
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

        let err = handle_invalidate(&state, AUTH, request).await.unwrap_err();
        assert!(matches!(err, AdminError::BadRequest(_)));
        assert_eq!(err.status(), 400);
    }

    #[tokio::test]
    async fn handlers_fail_closed_without_a_usable_configured_token() {
        for config in [
            AdminConfig::new().with_enabled(true), // require_auth, no token
            AdminConfig::new().with_enabled(true).with_auth_token(""), // empty token
            AdminConfig::new().with_enabled(true).with_auth_token("   "), // whitespace token
        ] {
            let state = AdminState::new(InMemoryBackend::new(10), config);
            for header in [
                None,
                Some("Bearer "),
                Some("Bearer    "),
                Some("Bearer x"),
                Some(""),
            ] {
                assert_eq!(
                    handle_stats(&state, header).unwrap_err(),
                    AdminError::Unauthorized
                );
                assert_eq!(
                    handle_hot_keys(&state, header, None).unwrap_err(),
                    AdminError::Unauthorized
                );
                assert_eq!(
                    handle_list_tags(&state, header).await.unwrap_err(),
                    AdminError::Unauthorized
                );
                let req = InvalidationRequest {
                    key: Some("k".into()),
                    tag: None,
                    tags: None,
                };
                assert_eq!(
                    handle_invalidate(&state, header, req).await.unwrap_err(),
                    AdminError::Unauthorized
                );
            }
        }
    }

    #[tokio::test]
    async fn require_auth_false_opens_enabled_handlers() {
        let state = AdminState::new(
            InMemoryBackend::new(10),
            AdminConfig::new()
                .with_enabled(true)
                .with_require_auth(false),
        );
        assert!(handle_stats(&state, None).is_ok());
        assert!(handle_hot_keys(&state, None, None).is_ok());
        assert!(handle_list_tags(&state, None).await.is_ok());
        let req = InvalidationRequest {
            key: Some("k".into()),
            tag: None,
            tags: None,
        };
        assert!(handle_invalidate(&state, None, req).await.is_ok());
    }

    #[tokio::test]
    async fn backend_errors_map_to_admin_error() {
        use crate::backend::CacheRead;
        use crate::error::CacheError;

        #[derive(Clone)]
        struct FailingBackend;

        impl CacheBackend for FailingBackend {
            async fn get(&self, _key: &str) -> Result<Option<CacheRead>, CacheError> {
                Err(CacheError::Backend("unavailable".to_string()))
            }

            async fn set(
                &self,
                _key: String,
                _entry: CacheEntry,
                _ttl: Duration,
                _stale_for: Duration,
            ) -> Result<(), CacheError> {
                Err(CacheError::Backend("unavailable".to_string()))
            }

            async fn invalidate(&self, _key: &str) -> Result<(), CacheError> {
                Err(CacheError::Backend("unavailable".to_string()))
            }

            async fn list_tags(&self) -> Result<Vec<String>, CacheError> {
                Err(CacheError::Unsupported("tags not supported".to_string()))
            }

            async fn invalidate_by_tag(&self, _tag: &str) -> Result<usize, CacheError> {
                Err(CacheError::Unsupported("tags not supported".to_string()))
            }

            async fn invalidate_by_tags(&self, _tags: &[String]) -> Result<usize, CacheError> {
                Err(CacheError::Unsupported("tags not supported".to_string()))
            }
        }

        let state = AdminState::new(
            FailingBackend,
            AdminConfig::new().with_enabled(true).with_auth_token(TOKEN),
        );

        let list_err = handle_list_tags(&state, AUTH).await.unwrap_err();
        assert!(matches!(list_err, AdminError::Unsupported(_)));
        assert_eq!(list_err.status(), 501);

        let key_err = handle_invalidate(
            &state,
            AUTH,
            InvalidationRequest {
                key: Some("k".into()),
                tag: None,
                tags: None,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(key_err, AdminError::Backend(_)));
        assert_eq!(key_err.status(), 500);

        for req in [
            InvalidationRequest {
                key: None,
                tag: Some("t".into()),
                tags: None,
            },
            InvalidationRequest {
                key: None,
                tag: None,
                tags: Some(vec!["t1".into()]),
            },
        ] {
            let err = handle_invalidate(&state, AUTH, req).await.unwrap_err();
            assert!(matches!(err, AdminError::Unsupported(_)));
            assert_eq!(err.status(), 501);
        }
    }

    #[test]
    fn error_response_from_admin_error_maps_variants() {
        let disabled = ErrorResponse::from(AdminError::Disabled);
        assert_eq!(disabled.status, 404);
        assert_eq!(disabled.error, "not found");

        let unauth = ErrorResponse::from(&AdminError::Unauthorized);
        assert_eq!(unauth.status, 401);
        assert_eq!(unauth.error, "unauthorized");

        let bad_req = ErrorResponse::from(AdminError::BadRequest("bad input".into()));
        assert_eq!(bad_req.status, 400);
        assert_eq!(bad_req.error, "bad input");

        let unsupported = ErrorResponse::from(AdminError::Unsupported("tags not supported".into()));
        assert_eq!(unsupported.status, 501);
        assert_eq!(unsupported.error, "tags not supported");

        let backend = ErrorResponse::from(&AdminError::Backend("backend down".into()));
        assert_eq!(backend.status, 500);
        assert_eq!(backend.error, "backend down");
    }

    #[test]
    fn validate_auth_with_bearer_token() {
        let config = AdminConfig::new()
            .with_auth_token("secret123")
            .with_require_auth(true);

        assert!(validate_auth(&config, Some("Bearer secret123")));
        assert!(validate_auth(&config, Some("bearer secret123")));
        assert!(validate_auth(&config, Some("BEARER secret123")));
        assert!(!validate_auth(&config, Some("secret123")));
        assert!(!validate_auth(&config, Some("Basic secret123")));
        assert!(!validate_auth(&config, Some("Bearer ")));
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
