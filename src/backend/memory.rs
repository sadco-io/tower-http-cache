use async_trait::async_trait;
use moka::future::Cache;
use std::time::{Duration, SystemTime};

use super::{CacheBackend, CacheEntry, CacheRead};
use crate::error::CacheError;

/// An in-memory [`CacheBackend`] implementation backed by [`moka`].
///
/// The backend is cheap to clone and shares a single underlying cache.
#[derive(Clone)]
pub struct InMemoryBackend {
    cache: Cache<String, StoredEntry>,
}

#[derive(Clone)]
struct StoredEntry {
    entry: CacheEntry,
    expires_at: SystemTime,
    stale_until: SystemTime,
}

impl InMemoryBackend {
    /// Creates a new in-memory cache with the provided `max_capacity`.
    ///
    /// The capacity is expressed in number of cached entries, not bytes.
    pub fn new(max_capacity: u64) -> Self {
        let cache = Cache::builder().max_capacity(max_capacity).build();
        Self { cache }
    }
}

#[async_trait]
impl CacheBackend for InMemoryBackend {
    async fn get(&self, key: &str) -> Result<Option<CacheRead>, CacheError> {
        if let Some(stored) = self.cache.get(key).await {
            let now = SystemTime::now();
            if now > stored.stale_until {
                self.cache.invalidate(key).await;
                return Ok(None);
            }

            Ok(Some(CacheRead {
                entry: stored.entry.clone(),
                expires_at: Some(stored.expires_at),
                stale_until: Some(stored.stale_until),
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

        let now = SystemTime::now();
        let expires_at = now + ttl;
        let stale_until = expires_at + stale_for;

        let stored = StoredEntry {
            entry,
            expires_at,
            stale_until,
        };
        self.cache.insert(key, stored).await;
        Ok(())
    }

    async fn invalidate(&self, key: &str) -> Result<(), CacheError> {
        self.cache.invalidate(key).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::CacheEntry;
    use bytes::Bytes;
    use http::{StatusCode, Version};
    use tokio::time::{sleep, Duration};

    fn entry_with_body(body: &'static [u8]) -> CacheEntry {
        CacheEntry::new(
            StatusCode::OK,
            Version::HTTP_11,
            Vec::new(),
            Bytes::from_static(body),
        )
    }

    #[tokio::test]
    async fn set_and_get_returns_cached_entry() {
        let backend = InMemoryBackend::new(16);
        let entry = entry_with_body(b"alpha");

        backend
            .set(
                "key".into(),
                entry.clone(),
                Duration::from_secs(1),
                Duration::from_secs(1),
            )
            .await
            .expect("set succeeds");

        let read = backend.get("key").await.expect("get succeeds");
        let cached = read.expect("entry present");

        assert_eq!(cached.entry.body, entry.body);
        assert!(cached.expires_at.is_some());
        assert!(cached.stale_until.is_some());
    }

    #[tokio::test]
    async fn entry_invalidated_after_stale_window() {
        let backend = InMemoryBackend::new(16);

        backend
            .set(
                "key".into(),
                entry_with_body(b"stale"),
                Duration::from_millis(20),
                Duration::from_millis(30),
            )
            .await
            .expect("set succeeds");

        sleep(Duration::from_millis(35)).await;
        let read = backend.get("key").await.expect("get succeeds");
        assert!(read.is_some(), "entry available during stale window");

        sleep(Duration::from_millis(40)).await;
        let read = backend.get("key").await.expect("get succeeds");
        assert!(read.is_none(), "entry removed after stale window");
    }
}
