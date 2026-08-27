//! Memcached cache backend implementation with connection pooling.
//!
//! This module provides a distributed caching backend using Memcached,
//! a high-performance, distributed memory caching system. Memcached is
//! particularly well-suited for:
//!
//! - Distributed caching across multiple servers
//! - High-throughput scenarios
//! - Simple key-value storage with TTL
//! - Memory-efficient caching at scale
//!
//! # Connection Pooling
//!
//! The backend uses bb8 connection pooling for efficient connection management:
//! - Configurable pool size (default: 10 connections)
//! - Automatic connection health checks
//! - Connection reuse for better performance
//! - Graceful failover handling
//!
//! # Example
//!
//! ```no_run
//! use tower_http_cache::backend::memcached::MemcachedBackend;
//! use std::time::Duration;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Simple setup with defaults
//! let backend = MemcachedBackend::new("127.0.0.1:11211").await?;
//!
//! // Advanced setup with builder
//! let backend = MemcachedBackend::builder()
//!     .address("127.0.0.1:11211")
//!     .namespace("myapp")
//!     .max_connections(20)
//!     .min_connections(5)
//!     .connection_timeout(Duration::from_secs(5))
//!     .build()
//!     .await?;
//! # Ok(())
//! # }
//! ```

use async_memcached::{AsciiProtocol, Client};
use async_trait::async_trait;
use bb8::{Pool, PooledConnection};
use std::time::Duration;

use super::{CacheBackend, CacheEntry, CacheRead};
use crate::codec::envelope::{self, LegacyShape};
use crate::codec::{CacheCodec, PostcardCodec};
use crate::error::CacheError;

/// Connection manager for bb8 pool.
///
/// Manages the lifecycle of Memcached connections including creation,
/// health checks, and cleanup.
pub struct MemcachedConnectionManager {
    address: String,
}

impl MemcachedConnectionManager {
    /// Creates a new connection manager for the given address.
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
        }
    }
}

#[async_trait]
impl bb8::ManageConnection for MemcachedConnectionManager {
    type Connection = Client;
    type Error = async_memcached::Error;

    async fn connect(&self) -> Result<Self::Connection, Self::Error> {
        Client::new(&self.address).await
    }

    async fn is_valid(&self, conn: &mut Self::Connection) -> Result<(), Self::Error> {
        // Simple health check: try to get the version
        conn.version().await?;
        Ok(())
    }

    fn has_broken(&self, _conn: &mut Self::Connection) -> bool {
        // Let is_valid handle health checking
        false
    }
}

type MemcachedPool = Pool<MemcachedConnectionManager>;

/// Memcached cache backend with connection pooling.
///
/// Provides distributed caching using the Memcached protocol with efficient
/// connection management via bb8 pooling. Entries are serialized by the
/// configured [`CacheCodec`] and wrapped in the shared
/// [envelope](crate::codec::envelope), the same format the Redis backend
/// writes.
#[derive(Clone)]
pub struct MemcachedBackend<C = PostcardCodec> {
    pool: MemcachedPool,
    namespace: String,
    codec: C,
}

impl MemcachedBackend<PostcardCodec> {
    /// Creates a new Memcached backend with default pool settings.
    ///
    /// The default pool configuration:
    /// - Max connections: 10
    /// - Min idle connections: 2
    /// - Connection timeout: 30 seconds
    ///
    /// # Arguments
    ///
    /// * `address` - The Memcached server address (e.g., "127.0.0.1:11211")
    ///
    /// # Errors
    ///
    /// Returns an error if the connection pool cannot be created or the
    /// initial connection fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use tower_http_cache::backend::memcached::MemcachedBackend;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let backend = MemcachedBackend::new("127.0.0.1:11211").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn new(address: impl Into<String>) -> Result<Self, CacheError> {
        Self::builder().address(address).build().await
    }

    /// Creates a builder for advanced configuration.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use tower_http_cache::backend::memcached::MemcachedBackend;
    /// # use std::time::Duration;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let backend = MemcachedBackend::builder()
    ///     .address("127.0.0.1:11211")
    ///     .namespace("myapp")
    ///     .max_connections(20)
    ///     .min_connections(5)
    ///     .connection_timeout(Duration::from_secs(5))
    ///     .build()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn builder() -> MemcachedBackendBuilder {
        MemcachedBackendBuilder::default()
    }
}

impl<C> MemcachedBackend<C> {
    /// Replaces the codec used to serialize entries.
    ///
    /// The codec's [`CacheCodec::CODEC_ID`] is recorded in the envelope
    /// header, so entries written by one codec are not handed to another.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use tower_http_cache::backend::memcached::MemcachedBackend;
    /// # use tower_http_cache::codec::PostcardCodec;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let backend = MemcachedBackend::new("127.0.0.1:11211")
    ///     .await?
    ///     .with_codec(PostcardCodec);
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_codec<NC>(self, codec: NC) -> MemcachedBackend<NC> {
        MemcachedBackend {
            pool: self.pool,
            namespace: self.namespace,
            codec,
        }
    }

    /// Gets a connection from the pool.
    ///
    /// # Errors
    ///
    /// Returns an error if no connection is available within the timeout period.
    async fn get_connection(
        &self,
    ) -> Result<PooledConnection<'_, MemcachedConnectionManager>, CacheError> {
        self.pool
            .get()
            .await
            .map_err(|e| CacheError::Backend(format!("Failed to get connection: {}", e)))
    }

    /// Constructs a namespaced cache key.
    fn make_key(&self, key: &str) -> String {
        format!("{}:{}", self.namespace, key)
    }

    /// Gets pool statistics.
    ///
    /// Returns information about the current state of the connection pool.
    pub fn pool_state(&self) -> PoolState {
        let state = self.pool.state();
        PoolState {
            connections: state.connections,
            idle_connections: state.idle_connections,
        }
    }
}

/// Connection pool state information.
#[derive(Debug, Clone)]
pub struct PoolState {
    /// Total number of connections in the pool
    pub connections: u32,
    /// Number of idle connections available
    pub idle_connections: u32,
}

/// Builder for configuring a Memcached backend.
///
/// Provides fine-grained control over connection pooling and backend behavior.
pub struct MemcachedBackendBuilder {
    address: Option<String>,
    namespace: String,
    max_connections: u32,
    min_connections: u32,
    connection_timeout: Duration,
}

impl Default for MemcachedBackendBuilder {
    fn default() -> Self {
        Self {
            address: None,
            namespace: "tower_http_cache".to_string(),
            max_connections: 10,
            min_connections: 2,
            connection_timeout: Duration::from_secs(30),
        }
    }
}

impl MemcachedBackendBuilder {
    /// Sets the Memcached server address.
    ///
    /// # Arguments
    ///
    /// * `address` - Server address (e.g., "127.0.0.1:11211")
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use tower_http_cache::backend::memcached::MemcachedBackend;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let backend = MemcachedBackend::builder()
    ///     .address("127.0.0.1:11211")
    ///     .build()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn address(mut self, address: impl Into<String>) -> Self {
        self.address = Some(address.into());
        self
    }

    /// Sets a custom namespace prefix for cache keys.
    ///
    /// This is useful for avoiding key collisions when multiple applications
    /// share the same Memcached instance.
    ///
    /// Default: "tower_http_cache"
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use tower_http_cache::backend::memcached::MemcachedBackend;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let backend = MemcachedBackend::builder()
    ///     .address("127.0.0.1:11211")
    ///     .namespace("myapp")
    ///     .build()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    /// Sets the maximum number of connections in the pool.
    ///
    /// Default: 10
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use tower_http_cache::backend::memcached::MemcachedBackend;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let backend = MemcachedBackend::builder()
    ///     .address("127.0.0.1:11211")
    ///     .max_connections(20)
    ///     .build()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn max_connections(mut self, max: u32) -> Self {
        self.max_connections = max;
        self
    }

    /// Sets the minimum number of idle connections to maintain.
    ///
    /// Default: 2
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use tower_http_cache::backend::memcached::MemcachedBackend;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let backend = MemcachedBackend::builder()
    ///     .address("127.0.0.1:11211")
    ///     .min_connections(5)
    ///     .build()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn min_connections(mut self, min: u32) -> Self {
        self.min_connections = min;
        self
    }

    /// Sets the connection timeout.
    ///
    /// This is the maximum time to wait when acquiring a connection from the pool.
    ///
    /// Default: 30 seconds
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use tower_http_cache::backend::memcached::MemcachedBackend;
    /// # use std::time::Duration;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let backend = MemcachedBackend::builder()
    ///     .address("127.0.0.1:11211")
    ///     .connection_timeout(Duration::from_secs(5))
    ///     .build()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn connection_timeout(mut self, timeout: Duration) -> Self {
        self.connection_timeout = timeout;
        self
    }

    /// Builds the Memcached backend.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No address was provided
    /// - The connection pool cannot be created
    /// - The initial connection fails
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use tower_http_cache::backend::memcached::MemcachedBackend;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let backend = MemcachedBackend::builder()
    ///     .address("127.0.0.1:11211")
    ///     .build()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn build(self) -> Result<MemcachedBackend<PostcardCodec>, CacheError> {
        let address = self
            .address
            .ok_or_else(|| CacheError::Backend("address is required".to_string()))?;

        let manager = MemcachedConnectionManager::new(address);

        let pool = Pool::builder()
            .max_size(self.max_connections)
            .min_idle(Some(self.min_connections))
            .connection_timeout(self.connection_timeout)
            .build(manager)
            .await
            .map_err(|e| CacheError::Backend(format!("Failed to create connection pool: {}", e)))?;

        Ok(MemcachedBackend {
            pool,
            namespace: self.namespace,
            codec: PostcardCodec,
        })
    }
}

#[async_trait]
impl<C> CacheBackend for MemcachedBackend<C>
where
    C: CacheCodec,
{
    async fn get(&self, key: &str) -> Result<Option<CacheRead>, CacheError> {
        let namespaced_key = self.make_key(key);
        let mut conn = self.get_connection().await?;

        let value = (*conn)
            .get(namespaced_key.as_bytes())
            .await
            .map_err(|e| CacheError::Backend(format!("Memcached get failed: {}", e)))?;

        if let Some(data) = value {
            // Extract the bytes from the Value
            let data_bytes = data
                .data
                .as_ref()
                .ok_or_else(|| CacheError::Backend("Memcached value has no data".to_string()))?;

            envelope::read_stored(
                data_bytes.as_slice(),
                &self.codec,
                LegacyShape::MemcachedOuter,
            )
        } else {
            Ok(None)
        }
    }

    async fn set(
        &self,
        key: String,
        entry: CacheEntry,
        ttl: Duration,
        stale_for: Duration,
    ) -> Result<(), CacheError> {
        if ttl.is_zero() {
            return Ok(());
        }

        let namespaced_key = self.make_key(&key);

        // Calculate expiration times
        let now_ms = envelope::current_millis()?;
        let expires_at_ms = now_ms.saturating_add(envelope::duration_millis(ttl));
        let stale_until_ms = expires_at_ms.saturating_add(envelope::duration_millis(stale_for));

        // Serialize the entry and wrap it in the shared envelope
        let payload = self.codec.encode(&entry)?;
        let bytes = envelope::wrap(C::CODEC_ID, expires_at_ms, stale_until_ms, &payload);

        // Memcached TTL is the total time (fresh + stale)
        let total_ttl = ttl.saturating_add(stale_for);
        let ttl_secs = total_ttl.as_secs();

        // Memcached TTL is u32 (max ~136 years)
        let ttl_u32 = ttl_secs.min(u32::MAX as u64) as u32;

        let mut conn = self.get_connection().await?;
        (*conn)
            .set(
                namespaced_key.as_bytes(),
                bytes.as_slice(),
                Some(ttl_u32 as i64),
                Default::default(),
            )
            .await
            .map_err(|e| CacheError::Backend(format!("Memcached set failed: {}", e)))?;

        Ok(())
    }

    async fn invalidate(&self, key: &str) -> Result<(), CacheError> {
        let namespaced_key = self.make_key(key);
        let mut conn = self.get_connection().await?;

        (*conn)
            .delete(namespaced_key.as_bytes())
            .await
            .map_err(|e| CacheError::Backend(format!("Memcached delete failed: {}", e)))?;

        Ok(())
    }

    /// Always [`CacheError::Unsupported`]: this backend keeps no tag index.
    ///
    /// 0.6.0 puts tags on the wire, so a `CacheRead` from this backend carries
    /// the tags the entry was stored with — but there is no reverse index, so
    /// tag -> keys cannot be answered. Reporting it is deliberate: inheriting
    /// the trait default would answer `Ok(vec![])`, and the caller could not
    /// tell that from "nothing carried that tag". A Redis-native index is
    /// planned for 0.7.0; memcached has no set type and will keep reporting
    /// this.
    async fn get_keys_by_tag(&self, _tag: &str) -> Result<Vec<String>, CacheError> {
        Err(unsupported_tags())
    }

    /// Always [`CacheError::Unsupported`]: this backend keeps no tag index.
    ///
    /// 0.6.0 puts tags on the wire, so a `CacheRead` from this backend carries
    /// the tags the entry was stored with — but there is no reverse index, so
    /// tag -> keys cannot be answered. Reporting it is deliberate: inheriting
    /// the trait default would answer `Ok(vec![])`, and the caller could not
    /// tell that from "nothing carried that tag". A Redis-native index is
    /// planned for 0.7.0; memcached has no set type and will keep reporting
    /// this.
    async fn list_tags(&self) -> Result<Vec<String>, CacheError> {
        Err(unsupported_tags())
    }
}

fn unsupported_tags() -> CacheError {
    CacheError::Unsupported(
        "MemcachedBackend keeps no tag index; tag lookup and tag invalidation \
         are not available on it. Memcached has no set type, so this is not \
         planned."
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::StatusCode;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn test_make_key() {
        // We can't easily create a MemcachedBackend without a connection,
        // so we'll test the key format directly
        let namespace = "test_app";
        let make_key = |key: &str| format!("{}:{}", namespace, key);

        assert_eq!(make_key("my_key"), "test_app:my_key");
        assert_eq!(make_key("another/key"), "test_app:another/key");
    }

    #[test]
    fn test_system_time_conversion() {
        let now = SystemTime::now();
        let ms = now.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        let converted = envelope::unix_ms_to_system_time(ms);

        // Should be within 1ms of each other
        let diff = now
            .duration_since(converted)
            .or_else(|_| converted.duration_since(now))
            .unwrap();
        assert!(diff.as_millis() < 2);
    }

    /// Round-trips an entry through the codec and the shared envelope, which
    /// is exactly what `set`/`get` do either side of the network. Replaces the
    /// old `test_memcached_record_serialization`, which asserted the fields it
    /// had just set and explicitly did not test serialization.
    #[test]
    fn record_round_trips_through_envelope() {
        let entry = CacheEntry::new(
            StatusCode::OK,
            http::Version::HTTP_11,
            vec![("content-type".to_string(), b"application/json".to_vec())],
            Bytes::from_static(b"{\"test\":true}"),
        )
        .with_tags(vec!["user:123".to_string(), "tenant:acme".to_string()]);

        let codec = PostcardCodec;
        let payload = codec.encode(&entry).unwrap();
        let bytes = envelope::wrap(PostcardCodec::CODEC_ID, 1_000_000, 2_000_000, &payload);

        let read = envelope::read_stored(&bytes, &codec, LegacyShape::MemcachedOuter)
            .unwrap()
            .expect("entry should decode");

        assert_eq!(read.entry.status, entry.status);
        assert_eq!(read.entry.version, entry.version);
        assert_eq!(read.entry.headers, entry.headers);
        assert_eq!(read.entry.body, entry.body);
        assert_eq!(read.entry.tags, entry.tags);
        assert_eq!(
            read.expires_at,
            Some(envelope::unix_ms_to_system_time(1_000_000))
        );
        assert_eq!(
            read.stale_until,
            Some(envelope::unix_ms_to_system_time(2_000_000))
        );
    }

    #[test]
    fn test_builder_defaults() {
        let builder = MemcachedBackendBuilder::default();
        assert_eq!(builder.namespace, "tower_http_cache");
        assert_eq!(builder.max_connections, 10);
        assert_eq!(builder.min_connections, 2);
        assert_eq!(builder.connection_timeout, Duration::from_secs(30));
    }

    #[test]
    fn test_builder_customization() {
        let builder = MemcachedBackendBuilder::default()
            .address("127.0.0.1:11211")
            .namespace("custom")
            .max_connections(20)
            .min_connections(5)
            .connection_timeout(Duration::from_secs(10));

        assert_eq!(builder.address, Some("127.0.0.1:11211".to_string()));
        assert_eq!(builder.namespace, "custom");
        assert_eq!(builder.max_connections, 20);
        assert_eq!(builder.min_connections, 5);
        assert_eq!(builder.connection_timeout, Duration::from_secs(10));
    }

    // Integration tests would require a running Memcached instance
    // They should be in the tests/ directory and marked with #[ignore]
}
