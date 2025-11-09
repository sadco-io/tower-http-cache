//! Storage backends for the cache layer.
//!
//! The cache layer requires a [`CacheBackend`] implementation to persist
//! cached responses. This module ships with:
//! - [`memory::InMemoryBackend`] — a fast, process-local cache backed by [`moka`].
//! - `redis::RedisBackend` *(optional)* — a distributed cache when the
//!   `redis-backend` crate feature is enabled.
//!
//! Backends are responsible for answering cache lookups, storing entries,
//! and enforcing per-entry stale windows.

pub mod memory;
#[cfg(feature = "redis-backend")]
pub mod redis;

use async_trait::async_trait;
use bytes::Bytes;
use http::{HeaderName, HeaderValue, Response, StatusCode, Version};
use std::time::{Duration, SystemTime};

use crate::error::CacheError;

/// Cached response payload captured by the cache layer.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub status: StatusCode,
    pub version: Version,
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Bytes,
}

impl CacheEntry {
    /// Creates a new cached response entry.
    ///
    /// The entry captures the response status, HTTP version, a serialized
    /// subset of headers, and the collected response body.
    pub fn new(
        status: StatusCode,
        version: Version,
        headers: Vec<(String, Vec<u8>)>,
        body: Bytes,
    ) -> Self {
        Self {
            status,
            version,
            headers,
            body,
        }
    }

    /// Converts the entry back into an `http::Response`.
    pub fn into_response(self) -> Response<http_body_util::Full<Bytes>> {
        let mut response = Response::new(http_body_util::Full::from(self.body));
        *response.status_mut() = self.status;
        *response.version_mut() = self.version;

        let headers = response.headers_mut();
        headers.clear();
        for (name, value) in self.headers {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_bytes(&value),
            ) {
                headers.append(name, value);
            }
        }

        response
    }
}

#[derive(Debug, Clone)]
pub struct CacheRead {
    /// Cached entry together with timing metadata.
    pub entry: CacheEntry,
    pub expires_at: Option<SystemTime>,
    pub stale_until: Option<SystemTime>,
}

#[async_trait]
pub trait CacheBackend: Send + Sync + Clone + 'static {
    /// Fetches a cached entry by key.
    ///
    /// Returns `Ok(None)` when the backend does not have a value or the
    /// entry has expired.
    async fn get(&self, key: &str) -> Result<Option<CacheRead>, CacheError>;
    /// Stores an entry with a time-to-live and additional stale window.
    async fn set(
        &self,
        key: String,
        entry: CacheEntry,
        ttl: Duration,
        stale_for: Duration,
    ) -> Result<(), CacheError>;
    /// Invalidates the cache entry for `key`, if present.
    async fn invalidate(&self, key: &str) -> Result<(), CacheError>;
}
