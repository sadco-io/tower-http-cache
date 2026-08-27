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

#[cfg(feature = "in-memory")]
pub mod memory;
pub mod multi_tier;
#[cfg(feature = "redis-backend")]
pub mod redis;

use bytes::Bytes;
use http::{HeaderName, HeaderValue, Response, StatusCode, Version};
use std::future::Future;
use std::time::{Duration, SystemTime};

use crate::error::CacheError;
use crate::layer::SyncBoxBody;

/// Cached response payload captured by the cache layer.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CacheEntry {
    #[cfg_attr(feature = "serde", serde(with = "status_code_serde"))]
    pub status: StatusCode,
    #[cfg_attr(feature = "serde", serde(with = "version_serde"))]
    pub version: Version,
    pub headers: Vec<(String, Vec<u8>)>,
    #[cfg_attr(feature = "serde", serde(with = "bytes_serde"))]
    pub body: Bytes,
    pub tags: Option<Vec<String>>,
}

// Custom serde helpers for http types
#[cfg(feature = "serde")]
mod status_code_serde {
    use http::StatusCode;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(status: &StatusCode, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        status.as_u16().serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<StatusCode, D::Error>
    where
        D: Deserializer<'de>,
    {
        let code = u16::deserialize(deserializer)?;
        StatusCode::from_u16(code).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "serde")]
mod version_serde {
    use http::Version;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(version: &Version, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // The `u8` annotation is load-bearing. Without it the literals default
        // to `i32`, so this wrote four bytes while `deserialize` below read
        // one -- which made `CacheEntry`'s derived impls unusable with any
        // non-self-describing format. See CHANGELOG 0.6.0.
        let v: u8 = match *version {
            Version::HTTP_09 => 0,
            Version::HTTP_10 => 1,
            Version::HTTP_11 => 2,
            Version::HTTP_2 => 3,
            Version::HTTP_3 => 4,
            _ => 5,
        };
        v.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Version, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = u8::deserialize(deserializer)?;
        Ok(match v {
            0 => Version::HTTP_09,
            1 => Version::HTTP_10,
            2 => Version::HTTP_11,
            3 => Version::HTTP_2,
            4 => Version::HTTP_3,
            _ => Version::HTTP_11, // Default fallback
        })
    }
}

#[cfg(feature = "serde")]
mod bytes_serde {
    use bytes::Bytes;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &Bytes, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Bytes, D::Error>
    where
        D: Deserializer<'de>,
    {
        let vec = Vec::<u8>::deserialize(deserializer)?;
        Ok(Bytes::from(vec))
    }
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
    pub fn into_response(self) -> Response<SyncBoxBody> {
        use http_body_util::BodyExt;

        let full_body = http_body_util::Full::from(self.body);
        let boxed_body = full_body
            .map_err(|never| -> Box<dyn std::error::Error + Send + Sync> { match never {} })
            .boxed();

        let mut response = Response::new(SyncBoxBody::new(boxed_body));
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

/// Storage a [`CacheLayer`](crate::layer::CacheLayer) can read and write.
///
/// Every method returns `impl Future<..> + Send` rather than being an
/// `async fn`. The `+ Send` is required because the cache layer boxes backend
/// futures into a `Send` future; a bare `async fn` in a trait does not
/// guarantee it at the bound site.
///
/// **Implementing this trait is unaffected by that.** Write plain `async fn`
/// in your impl block, with no `#[async_trait]` attribute:
///
/// ```
/// # use std::time::Duration;
/// # use tower_http_cache::backend::{CacheBackend, CacheEntry, CacheRead};
/// # use tower_http_cache::error::CacheError;
/// #[derive(Clone)]
/// struct MyBackend;
///
/// impl CacheBackend for MyBackend {
///     async fn get(&self, _key: &str) -> Result<Option<CacheRead>, CacheError> {
///         Ok(None)
///     }
///     async fn set(
///         &self,
///         _key: String,
///         _entry: CacheEntry,
///         _ttl: Duration,
///         _stale_for: Duration,
///     ) -> Result<(), CacheError> {
///         Ok(())
///     }
///     async fn invalidate(&self, _key: &str) -> Result<(), CacheError> {
///         Ok(())
///     }
/// }
/// ```
pub trait CacheBackend: Send + Sync + Clone + 'static {
    /// Fetches a cached entry by key.
    ///
    /// Returns `Ok(None)` when the backend does not have a value or the
    /// entry has expired.
    fn get(&self, key: &str) -> impl Future<Output = Result<Option<CacheRead>, CacheError>> + Send;

    /// Stores an entry with a time-to-live and additional stale window.
    fn set(
        &self,
        key: String,
        entry: CacheEntry,
        ttl: Duration,
        stale_for: Duration,
    ) -> impl Future<Output = Result<(), CacheError>> + Send;

    /// Invalidates the cache entry for `key`, if present.
    fn invalidate(&self, key: &str) -> impl Future<Output = Result<(), CacheError>> + Send;

    /// Retrieves all cache keys associated with a tag.
    ///
    /// Returns an empty vector if tags are not supported by this backend.
    fn get_keys_by_tag(
        &self,
        _tag: &str,
    ) -> impl Future<Output = Result<Vec<String>, CacheError>> + Send {
        async { Ok(Vec::new()) }
    }

    /// Invalidates all cache entries associated with a tag.
    ///
    /// Returns the number of entries invalidated.
    fn invalidate_by_tag(
        &self,
        tag: &str,
    ) -> impl Future<Output = Result<usize, CacheError>> + Send {
        async move {
            let keys = self.get_keys_by_tag(tag).await?;
            let count = keys.len();
            for key in keys {
                let _ = self.invalidate(&key).await;
            }
            Ok(count)
        }
    }

    /// Invalidates all cache entries associated with multiple tags.
    ///
    /// Returns the total number of entries invalidated (may include duplicates).
    fn invalidate_by_tags(
        &self,
        tags: &[String],
    ) -> impl Future<Output = Result<usize, CacheError>> + Send {
        async move {
            let mut total = 0;
            for tag in tags {
                total += self.invalidate_by_tag(tag).await?;
            }
            Ok(total)
        }
    }

    /// Lists all currently indexed tags.
    ///
    /// Returns an empty vector if tags are not supported by this backend.
    fn list_tags(&self) -> impl Future<Output = Result<Vec<String>, CacheError>> + Send {
        async { Ok(Vec::new()) }
    }
}
