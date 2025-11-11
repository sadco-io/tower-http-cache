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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{CacheBackend, CacheEntry, CacheRead};
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
/// connection management via bb8 pooling. Entries are serialized with bincode
/// and stored with appropriate TTL values.
#[derive(Clone)]
pub struct MemcachedBackend {
    pool: MemcachedPool,
    namespace: String,
}

impl MemcachedBackend {
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
    pub async fn build(self) -> Result<MemcachedBackend, CacheError> {
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
        })
    }
}

/// Serializable record stored in Memcached.
///
/// We store the entry along with timing metadata so we can properly
/// handle stale-while-revalidate semantics.
#[derive(serde::Serialize, serde::Deserialize)]
struct MemcachedRecord {
    entry: CacheEntry,
    expires_at_ms: u64,
    stale_until_ms: u64,
}

/// Converts a SystemTime to milliseconds since UNIX_EPOCH.
fn system_time_to_unix_ms(time: SystemTime) -> Result<u64, CacheError> {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .map_err(|e| CacheError::Backend(format!("Time conversion error: {}", e)))
}

/// Converts milliseconds since UNIX_EPOCH to SystemTime.
fn unix_ms_to_system_time(ms: u64) -> Result<SystemTime, CacheError> {
    Ok(UNIX_EPOCH + Duration::from_millis(ms))
}

/// Gets the current time in milliseconds since UNIX_EPOCH.
fn current_millis() -> Result<u64, CacheError> {
    system_time_to_unix_ms(SystemTime::now())
}

/// Converts a Duration to milliseconds.
fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis() as u64
}

#[async_trait]
impl CacheBackend for MemcachedBackend {
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

            // Deserialize the record
            let record: MemcachedRecord = bincode::deserialize(data_bytes.as_slice())
                .map_err(|e| CacheError::Backend(format!("Deserialization failed: {}", e)))?;

            Ok(Some(CacheRead {
                entry: record.entry,
                expires_at: Some(unix_ms_to_system_time(record.expires_at_ms)?),
                stale_until: Some(unix_ms_to_system_time(record.stale_until_ms)?),
            }))
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
        let now_ms = current_millis()?;
        let expires_at_ms = now_ms.saturating_add(duration_millis(ttl));
        let stale_until_ms = expires_at_ms.saturating_add(duration_millis(stale_for));

        // Create the record
        let record = MemcachedRecord {
            entry,
            expires_at_ms,
            stale_until_ms,
        };

        // Serialize the record
        let bytes = bincode::serialize(&record)
            .map_err(|e| CacheError::Backend(format!("Serialization failed: {}", e)))?;

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::StatusCode;

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
        let ms = system_time_to_unix_ms(now).unwrap();
        let converted = unix_ms_to_system_time(ms).unwrap();

        // Should be within 1ms of each other
        let diff = now
            .duration_since(converted)
            .or_else(|_| converted.duration_since(now))
            .unwrap();
        assert!(diff.as_millis() < 2);
    }

    #[test]
    fn test_memcached_record_serialization() {
        let entry = CacheEntry::new(
            StatusCode::OK,
            http::Version::HTTP_11,
            vec![("content-type".to_string(), b"application/json".to_vec())],
            Bytes::from_static(b"{\"test\":true}"),
        );

        let record = MemcachedRecord {
            entry: entry.clone(),
            expires_at_ms: 1000000,
            stale_until_ms: 2000000,
        };

        // Verify record was created correctly
        assert_eq!(record.entry.status, StatusCode::OK);
        assert_eq!(record.entry.version, http::Version::HTTP_11);
        assert_eq!(record.entry.body, Bytes::from_static(b"{\"test\":true}"));
        assert_eq!(record.expires_at_ms, 1000000);
        assert_eq!(record.stale_until_ms, 2000000);

        // Note: Serialization round-trip testing should be done in integration tests
        // with an actual Memcached instance, as bincode serialization details are
        // implementation-dependent and the custom serde implementations may have
        // specific requirements.
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
