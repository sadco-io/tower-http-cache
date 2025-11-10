//! Admin API for cache introspection and management.
//!
//! This module provides HTTP endpoints for monitoring and managing the cache
//! at runtime. Features include statistics, key inspection, hot key tracking,
//! and tag-based invalidation.

pub mod routes;
pub mod stats;

use crate::backend::CacheBackend;
use std::sync::Arc;

/// Configuration for the admin API.
#[derive(Debug, Clone)]
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

    /// Validates the authentication token.
    pub fn validate_token(&self, provided: Option<&str>) -> bool {
        if !self.require_auth {
            return true;
        }

        match (&self.auth_token, provided) {
            (Some(expected), Some(provided)) => expected == provided,
            (None, _) => !self.require_auth,
            _ => false,
        }
    }
}

/// Shared state for admin API handlers.
#[derive(Clone)]
pub struct AdminState<B: CacheBackend> {
    pub backend: B,
    pub config: Arc<AdminConfig>,
    pub stats: Arc<stats::GlobalStats>,
}

impl<B: CacheBackend> AdminState<B> {
    /// Creates new admin state.
    pub fn new(backend: B, config: AdminConfig) -> Self {
        Self {
            backend,
            config: Arc::new(config),
            stats: Arc::new(stats::GlobalStats::new()),
        }
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
}
