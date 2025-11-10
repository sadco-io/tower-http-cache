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
pub mod multi_tier;
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
    pub tags: Option<Vec<String>>,
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
            tags: None,
        }
    }

    /// Creates a new cached response entry with tags.
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = Some(tags);
        self
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

    /// Retrieves all cache keys associated with a tag.
    ///
    /// Returns an empty vector if tags are not supported by this backend.
    async fn get_keys_by_tag(&self, _tag: &str) -> Result<Vec<String>, CacheError> {
        Ok(Vec::new())
    }

    /// Invalidates all cache entries associated with a tag.
    ///
    /// Returns the number of entries invalidated.
    async fn invalidate_by_tag(&self, tag: &str) -> Result<usize, CacheError> {
        let keys = self.get_keys_by_tag(tag).await?;
        let count = keys.len();
        for key in keys {
            let _ = self.invalidate(&key).await;
        }
        Ok(count)
    }

    /// Invalidates all cache entries associated with multiple tags.
    ///
    /// Returns the total number of entries invalidated (may include duplicates).
    async fn invalidate_by_tags(&self, tags: &[String]) -> Result<usize, CacheError> {
        let mut total = 0;
        for tag in tags {
            total += self.invalidate_by_tag(tag).await?;
        }
        Ok(total)
    }

    /// Lists all currently indexed tags.
    ///
    /// Returns an empty vector if tags are not supported by this backend.
    async fn list_tags(&self) -> Result<Vec<String>, CacheError> {
        Ok(Vec::new())
    }
}
