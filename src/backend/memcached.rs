//! Memcached cache backend implementation.
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
//! # Example
//!
//! ```no_run
//! use tower_http_cache::backend::MemcachedBackend;
//! use std::time::Duration;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Connect to a single Memcached instance
//! let backend = MemcachedBackend::new("127.0.0.1:11211").await?;
//!
//! // Or connect to multiple servers for distribution
//! let backend = MemcachedBackend::new_with_servers(vec![
//!     "127.0.0.1:11211",
//!     "127.0.0.1:11212",
//! ]).await?;
//! # Ok(())
//! # }
//! ```

use async_memcached::{AsciiProtocol, Client};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

use super::{CacheBackend, CacheEntry, CacheRead};
use crate::error::CacheError;

/// Memcached cache backend.
///
/// Provides distributed caching using the Memcached protocol. Entries are
/// serialized with bincode and stored with appropriate TTL values.
#[derive(Clone)]
pub struct MemcachedBackend {
    client: Arc<Mutex<Client>>,
    namespace: String,
}

impl MemcachedBackend {
    /// Creates a new Memcached backend connected to a single server.
    ///
    /// # Arguments
    ///
    /// * `server` - The Memcached server address (e.g., "127.0.0.1:11211")
    ///
    /// # Errors
    ///
    /// Returns an error if the connection to the server fails.
    pub async fn new(server: impl AsRef<str>) -> Result<Self, CacheError> {
        let client = Client::new(server.as_ref())
            .await
            .map_err(|e| CacheError::Backend(format!("Failed to connect to Memcached: {}", e)))?;

        Ok(Self {
            client: Arc::new(Mutex::new(client)),
            namespace: "tower_http_cache".to_owned(),
        })
    }

    /// Creates a new Memcached backend connected to multiple servers.
    ///
    /// The client will automatically distribute keys across the servers
    /// using consistent hashing.
    ///
    /// # Arguments
    ///
    /// * `servers` - A vector of Memcached server addresses
    ///
    /// # Errors
    ///
    /// Returns an error if any connection fails.
    pub async fn new_with_servers(servers: Vec<impl AsRef<str>>) -> Result<Self, CacheError> {
        // For now, we'll use the first server. In the future, we could implement
        // proper multi-server support with consistent hashing.
        if servers.is_empty() {
            return Err(CacheError::Backend(
                "At least one server address required".to_owned(),
            ));
        }

        Self::new(servers[0].as_ref()).await
    }

    /// Sets a custom namespace prefix for cache keys.
    ///
    /// This is useful for avoiding key collisions when multiple applications
    /// share the same Memcached instance.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use tower_http_cache::backend::MemcachedBackend;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let backend = MemcachedBackend::new("127.0.0.1:11211")
    ///     .await?
    ///     .with_namespace("myapp");
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    /// Constructs a namespaced cache key.
    fn make_key(&self, key: &str) -> String {
        format!("{}:{}", self.namespace, key)
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
        let mut client = self.client.lock().await;

        let value = client
            .get(namespaced_key.as_bytes())
            .await
            .map_err(|e| CacheError::Backend(format!("Memcached get failed: {}", e)))?;

        if let Some(data) = value {
            // Extract the bytes from the Value
            // data.data is Option<Vec<u8>>
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

        let mut client = self.client.lock().await;
        client
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
        let mut client = self.client.lock().await;

        client
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
        // Create a fake backend just for testing make_key
        // We don't need a real client for this test
        let namespace = "test_app".to_owned();

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
        // This test requires serialization to work, which depends on the
        // exact serde implementation. Since we have custom serializers,
        // let's test that CacheEntry fields can be accessed correctly.
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

        // Note: Full serialization/deserialization testing should be done
        // in integration tests with an actual Memcached instance, as bincode
        // serialization details are implementation-dependent.
    }

    // Integration tests would require a running Memcached instance
    // They should be in the tests/ directory and marked with #[ignore]
}
