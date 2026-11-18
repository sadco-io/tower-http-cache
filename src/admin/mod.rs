//! Admin API for cache introspection and management.
//!
//! This module provides HTTP endpoints for monitoring and managing the cache
//! at runtime. Features include statistics, key inspection, hot key tracking,
//! and tag-based invalidation.

pub mod routes;
pub mod stats;

use crate::backend::CacheBackend;
use crate::chunks::ChunkCache;
use crate::layer::CacheLayer;
use sha2::{Digest, Sha256};
use std::fmt;
use std::sync::Arc;

/// Configuration for the admin API.
#[derive(Clone)]
pub struct AdminConfig {
    /// Whether admin API is enabled
    pub enabled: bool,

    /// Optional authentication token (Bearer token)
    pub auth_token: Option<String>,

    /// Whether authentication is required
    pub require_auth: bool,

    /// Mount path for admin endpoints (e.g., "/admin/cache")
    pub mount_path: String,

    /// Maximum number of keys to return in listing endpoints
    pub max_keys_per_request: usize,

    /// Maximum number of hot keys to track
    pub max_hot_keys: usize,
}

impl fmt::Debug for AdminConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdminConfig")
            .field("enabled", &self.enabled)
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|_| "[redacted]"),
            )
            .field("require_auth", &self.require_auth)
            .field("mount_path", &self.mount_path)
            .field("max_keys_per_request", &self.max_keys_per_request)
            .field("max_hot_keys", &self.max_hot_keys)
            .finish()
    }
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            auth_token: None,
            require_auth: true,
            mount_path: "/admin/cache".to_string(),
            max_keys_per_request: 100,
            max_hot_keys: 20,
        }
    }
}

impl AdminConfig {
    /// Creates a new admin configuration with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enables the admin API.
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Sets the authentication token.
    pub fn with_auth_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    /// Sets whether authentication is required.
    pub fn with_require_auth(mut self, require: bool) -> Self {
        self.require_auth = require;
        self
    }

    /// Sets the mount path.
    pub fn with_mount_path(mut self, path: impl Into<String>) -> Self {
        self.mount_path = path.into();
        self
    }

    /// Validates the authentication token in constant time over SHA-256 digests
    /// so neither token length nor prefix matching leaks timing information.
    pub fn validate_token(&self, provided: Option<&str>) -> bool {
        if !self.require_auth {
            return true;
        }

        match (&self.auth_token, provided) {
            (Some(expected), Some(provided)) => {
                if expected.trim().is_empty() || provided.trim().is_empty() {
                    return false;
                }
                constant_time_eq(expected.as_bytes(), provided.as_bytes())
            }
            (None, _) => false,
            _ => false,
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let digest_a = Sha256::digest(a);
    let digest_b = Sha256::digest(b);
    let mut diff = 0u8;
    for (&x, &y) in digest_a.iter().zip(digest_b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff == 0)
}

/// Shared state for admin API handlers.
#[derive(Clone)]
pub struct AdminState<B: CacheBackend> {
    pub backend: B,
    pub config: Arc<AdminConfig>,
    pub stats: Arc<stats::GlobalStats>,
    pub chunk_cache: Option<Arc<ChunkCache>>,
}

impl<B: CacheBackend> AdminState<B> {
    /// Creates new admin state with an isolated [`stats::GlobalStats`] tracker.
    pub fn new(backend: B, config: AdminConfig) -> Self {
        Self::with_stats(backend, config, Arc::new(stats::GlobalStats::new()))
    }

    /// Creates new admin state sharing an existing [`stats::GlobalStats`] tracker
    /// (for example obtained from `CacheLayer::stats()`).
    pub fn with_stats(backend: B, config: AdminConfig, stats: Arc<stats::GlobalStats>) -> Self {
        Self {
            backend,
            config: Arc::new(config),
            stats,
            chunk_cache: None,
        }
    }

    /// Attaches a chunk cache so admin invalidations also evict cached chunks.
    pub fn with_chunk_cache(mut self, chunk_cache: Arc<ChunkCache>) -> Self {
        self.chunk_cache = Some(chunk_cache);
        self
    }

    /// Builds admin state wired to the backend, shared stats, and optional chunk cache of a [`CacheLayer`].
    pub fn from_layer(layer: &CacheLayer<B>, config: AdminConfig) -> Self
    where
        B: Clone,
    {
        let mut state = Self::with_stats(layer.backend().clone(), config, layer.stats());
        if let Some(chunk_cache) = layer.chunk_cache().cloned() {
            state = state.with_chunk_cache(chunk_cache);
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_config_default() {
        let config = AdminConfig::default();
        assert!(!config.enabled);
        assert!(config.require_auth);
        assert_eq!(config.mount_path, "/admin/cache");
    }

    #[test]
    fn admin_config_builder() {
        let config = AdminConfig::new()
            .with_enabled(true)
            .with_auth_token("secret")
            .with_require_auth(false)
            .with_mount_path("/cache/admin");

        assert!(config.enabled);
        assert_eq!(config.auth_token, Some("secret".to_string()));
        assert!(!config.require_auth);
        assert_eq!(config.mount_path, "/cache/admin");
    }

    #[test]
    fn validate_token_success() {
        let config = AdminConfig::new()
            .with_auth_token("secret")
            .with_require_auth(true);

        assert!(config.validate_token(Some("secret")));
    }

    #[test]
    fn validate_token_failure() {
        let config = AdminConfig::new()
            .with_auth_token("secret")
            .with_require_auth(true);

        assert!(!config.validate_token(Some("wrong")));
        assert!(!config.validate_token(None));
    }

    #[test]
    fn validate_token_no_auth_required() {
        let config = AdminConfig::new().with_require_auth(false);

        assert!(config.validate_token(None));
        assert!(config.validate_token(Some("anything")));
    }

    #[test]
    fn admin_config_debug_redacts_auth_token() {
        let with_token = AdminConfig::new().with_auth_token("super-secret-token");
        let debug_str = format!("{with_token:?}");
        assert!(!debug_str.contains("super-secret-token"));
        assert!(debug_str.contains("[redacted]"));

        let without_token = AdminConfig::new();
        let debug_none = format!("{without_token:?}");
        assert!(debug_none.contains("auth_token: None"));
    }

    #[test]
    fn validate_token_rejects_prefix_and_different_lengths() {
        let config = AdminConfig::new()
            .with_auth_token("correct-horse-battery-staple")
            .with_require_auth(true);

        assert!(!config.validate_token(Some("correct-horse")));
        assert!(!config.validate_token(Some("correct-horse-battery-staple-extra")));
        assert!(!config.validate_token(Some("")));
        assert!(config.validate_token(Some("correct-horse-battery-staple")));
    }

    #[test]
    fn validate_token_rejects_empty_and_whitespace_configured_tokens() {
        let empty = AdminConfig::new()
            .with_auth_token("")
            .with_require_auth(true);
        assert!(!empty.validate_token(Some("")));
        assert!(!empty.validate_token(Some("   ")));
        assert!(!empty.validate_token(None));

        let whitespace = AdminConfig::new()
            .with_auth_token("   ")
            .with_require_auth(true);
        assert!(!whitespace.validate_token(Some("   ")));
        assert!(!whitespace.validate_token(Some("")));
    }
}
