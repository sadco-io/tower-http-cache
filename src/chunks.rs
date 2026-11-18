//! Chunk-based caching for large files and efficient range request handling.
//!
//! This module provides a chunk-based caching system that splits large files into
//! fixed-size chunks, enabling efficient storage and retrieval of partial content.
//! This is particularly useful for:
//! - Video streaming with range requests
//! - Large file downloads with resume support
//! - Efficient memory usage for large responses
//!
//! # Examples
//!
//! ```rust,no_run
//! use tower_http_cache::chunks::{ChunkCache, ChunkMetadata};
//! use bytes::Bytes;
//! use http::StatusCode;
//!
//! # async fn example() {
//! let chunk_cache = ChunkCache::new(1024 * 1024); // 1MB chunks
//!
//! let metadata = ChunkMetadata {
//!     total_size: 10 * 1024 * 1024, // 10MB file
//!     content_type: "video/mp4".to_string(),
//!     etag: Some("abc123".to_string()),
//!     last_modified: None,
//!     status: StatusCode::OK,
//!     version: http::Version::HTTP_11,
//!     headers: vec![],
//! };
//!
//! let entry = chunk_cache.get_or_create("video.mp4".to_string(), metadata);
//!
//! // Add chunks as they're received
//! entry.add_chunk(0, Bytes::from(vec![0u8; 1024 * 1024]));
//! entry.add_chunk(1, Bytes::from(vec![1u8; 1024 * 1024]));
//!
//! // Retrieve a range
//! let range = entry.get_range(0, 1024 * 1024 - 1);
//! assert!(range.is_some());
//! # }
//! ```

use bytes::Bytes;
use dashmap::DashMap;
use http::{StatusCode, Version};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// Chunk-based cache entry for large files.
///
/// A `ChunkedEntry` splits a large file into fixed-size chunks, allowing
/// efficient storage and retrieval of partial content. This is particularly
/// useful for range requests where only a portion of the file is needed.
#[derive(Debug, Clone)]
pub struct ChunkedEntry {
    /// Metadata about the full file
    pub metadata: ChunkMetadata,

    /// Map of chunk index to chunk data
    pub chunks: Arc<DashMap<u64, Bytes>>,

    /// Chunk size in bytes (default: 1MB)
    pub chunk_size: usize,

    /// Optional expiration instant for TTL enforcement.
    expires_at: Option<SystemTime>,

    /// Monotonic access sequence number for LRU eviction.
    last_accessed_seq: Arc<AtomicU64>,
}

/// Metadata for a chunked cache entry.
///
/// Contains HTTP response metadata without the body, which is stored
/// separately in chunks.
#[derive(Debug, Clone)]
pub struct ChunkMetadata {
    /// Total size of the file in bytes
    pub total_size: u64,

    /// Content-Type header value
    pub content_type: String,

    /// ETag header value, if present
    pub etag: Option<String>,

    /// Last-Modified header value, if present
    pub last_modified: Option<String>,

    /// HTTP status code
    pub status: StatusCode,

    /// HTTP version
    pub version: Version,

    /// Additional headers (name, value) pairs
    pub headers: Vec<(String, Vec<u8>)>,
}

impl ChunkedEntry {
    /// Creates a new chunked entry with the given metadata and chunk size.
    ///
    /// # Arguments
    ///
    /// * `metadata` - File metadata including size, content type, and headers
    /// * `chunk_size` - Size of each chunk in bytes
    ///
    /// # Examples
    ///
    /// ```rust
    /// use tower_http_cache::chunks::{ChunkedEntry, ChunkMetadata};
    /// use http::StatusCode;
    ///
    /// let metadata = ChunkMetadata {
    ///     total_size: 10_000_000,
    ///     content_type: "video/mp4".to_string(),
    ///     etag: None,
    ///     last_modified: None,
    ///     status: StatusCode::OK,
    ///     version: http::Version::HTTP_11,
    ///     headers: vec![],
    /// };
    ///
    /// let entry = ChunkedEntry::new(metadata, 1024 * 1024);
    /// assert_eq!(entry.chunk_size, 1024 * 1024);
    /// ```
    pub fn new(metadata: ChunkMetadata, chunk_size: usize) -> Self {
        Self::new_with_expiry(metadata, chunk_size, None)
    }

    fn new_with_expiry(
        metadata: ChunkMetadata,
        chunk_size: usize,
        expires_at: Option<SystemTime>,
    ) -> Self {
        Self {
            metadata,
            chunks: Arc::new(DashMap::new()),
            chunk_size,
            expires_at,
            last_accessed_seq: Arc::new(AtomicU64::new(0)),
        }
    }

    fn touch(&self, seq: u64) {
        self.last_accessed_seq.store(seq, Ordering::Relaxed);
    }

    fn stored_bytes(&self) -> usize {
        self.chunks.iter().map(|c| c.value().len()).sum()
    }

    /// Returns `true` if this entry has a TTL and has expired.
    pub fn is_expired(&self) -> bool {
        self.expires_at.is_some_and(|exp| SystemTime::now() >= exp)
    }

    /// Adds a chunk to the entry.
    ///
    /// # Arguments
    ///
    /// * `index` - Chunk index (0-based)
    /// * `data` - Chunk data
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use tower_http_cache::chunks::{ChunkedEntry, ChunkMetadata};
    /// # use bytes::Bytes;
    /// # use http::StatusCode;
    /// # let metadata = ChunkMetadata {
    /// #     total_size: 1024,
    /// #     content_type: "text/plain".to_string(),
    /// #     etag: None,
    /// #     last_modified: None,
    /// #     status: StatusCode::OK,
    /// #     version: http::Version::HTTP_11,
    /// #     headers: vec![],
    /// # };
    /// let entry = ChunkedEntry::new(metadata, 512);
    /// entry.add_chunk(0, Bytes::from("Hello, world!"));
    /// ```
    pub fn add_chunk(&self, index: u64, data: Bytes) {
        self.chunks.insert(index, data);
    }

    /// Gets a chunk by index.
    ///
    /// Returns `None` if the chunk is not cached.
    ///
    /// # Arguments
    ///
    /// * `index` - Chunk index (0-based)
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use tower_http_cache::chunks::{ChunkedEntry, ChunkMetadata};
    /// # use bytes::Bytes;
    /// # use http::StatusCode;
    /// # let metadata = ChunkMetadata {
    /// #     total_size: 1024,
    /// #     content_type: "text/plain".to_string(),
    /// #     etag: None,
    /// #     last_modified: None,
    /// #     status: StatusCode::OK,
    /// #     version: http::Version::HTTP_11,
    /// #     headers: vec![],
    /// # };
    /// let entry = ChunkedEntry::new(metadata, 512);
    /// entry.add_chunk(0, Bytes::from("Hello"));
    ///
    /// let chunk = entry.get_chunk(0);
    /// assert!(chunk.is_some());
    /// assert_eq!(chunk.unwrap(), "Hello");
    /// ```
    pub fn get_chunk(&self, index: u64) -> Option<Bytes> {
        self.chunks.get(&index).map(|r| r.value().clone())
    }

    /// Gets a byte range from the cached chunks.
    ///
    /// Returns `None` if any required chunks are missing from the cache.
    /// The range is inclusive: [start, end].
    ///
    /// # Arguments
    ///
    /// * `start` - Starting byte offset (inclusive)
    /// * `end` - Ending byte offset (inclusive)
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use tower_http_cache::chunks::{ChunkedEntry, ChunkMetadata};
    /// # use bytes::Bytes;
    /// # use http::StatusCode;
    /// # let metadata = ChunkMetadata {
    /// #     total_size: 2048,
    /// #     content_type: "text/plain".to_string(),
    /// #     etag: None,
    /// #     last_modified: None,
    /// #     status: StatusCode::OK,
    /// #     version: http::Version::HTTP_11,
    /// #     headers: vec![],
    /// # };
    /// let entry = ChunkedEntry::new(metadata, 1024);
    /// entry.add_chunk(0, Bytes::from(vec![0u8; 1024]));
    /// entry.add_chunk(1, Bytes::from(vec![1u8; 1024]));
    ///
    /// // Get first 512 bytes
    /// let range = entry.get_range(0, 511);
    /// assert!(range.is_some());
    /// assert_eq!(range.unwrap().len(), 512);
    /// ```
    pub fn get_range(&self, start: u64, end: u64) -> Option<Bytes> {
        let start_chunk = start / self.chunk_size as u64;
        let end_chunk = end / self.chunk_size as u64;

        let mut result = Vec::new();

        for chunk_idx in start_chunk..=end_chunk {
            let chunk = self.get_chunk(chunk_idx)?;

            // Calculate byte offsets within this chunk
            let chunk_start = chunk_idx * self.chunk_size as u64;
            let chunk_end = chunk_start + chunk.len() as u64;

            let data_start = if start > chunk_start {
                (start - chunk_start) as usize
            } else {
                0
            };

            let data_end = if end < chunk_end {
                (end - chunk_start + 1) as usize
            } else {
                chunk.len()
            };

            result.extend_from_slice(&chunk[data_start..data_end]);
        }

        Some(Bytes::from(result))
    }

    /// Checks if all chunks are cached.
    ///
    /// Returns `true` if the entry contains all chunks needed to represent
    /// the full file.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use tower_http_cache::chunks::{ChunkedEntry, ChunkMetadata};
    /// # use bytes::Bytes;
    /// # use http::StatusCode;
    /// # let metadata = ChunkMetadata {
    /// #     total_size: 2048,
    /// #     content_type: "text/plain".to_string(),
    /// #     etag: None,
    /// #     last_modified: None,
    /// #     status: StatusCode::OK,
    /// #     version: http::Version::HTTP_11,
    /// #     headers: vec![],
    /// # };
    /// let entry = ChunkedEntry::new(metadata, 1024);
    /// assert!(!entry.is_complete());
    ///
    /// entry.add_chunk(0, Bytes::from(vec![0u8; 1024]));
    /// entry.add_chunk(1, Bytes::from(vec![1u8; 1024]));
    /// assert!(entry.is_complete());
    /// ```
    pub fn is_complete(&self) -> bool {
        let total_chunks = self.metadata.total_size.div_ceil(self.chunk_size as u64);

        self.chunks.len() as u64 == total_chunks
    }

    /// Gets the cache coverage percentage.
    ///
    /// Returns a percentage (0-100) indicating how much of the file is cached.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use tower_http_cache::chunks::{ChunkedEntry, ChunkMetadata};
    /// # use bytes::Bytes;
    /// # use http::StatusCode;
    /// # let metadata = ChunkMetadata {
    /// #     total_size: 4096,
    /// #     content_type: "text/plain".to_string(),
    /// #     etag: None,
    /// #     last_modified: None,
    /// #     status: StatusCode::OK,
    /// #     version: http::Version::HTTP_11,
    /// #     headers: vec![],
    /// # };
    /// let entry = ChunkedEntry::new(metadata, 1024);
    /// assert_eq!(entry.coverage(), 0.0);
    ///
    /// entry.add_chunk(0, Bytes::from(vec![0u8; 1024]));
    /// assert_eq!(entry.coverage(), 25.0);
    ///
    /// entry.add_chunk(1, Bytes::from(vec![1u8; 1024]));
    /// assert_eq!(entry.coverage(), 50.0);
    /// ```
    pub fn coverage(&self) -> f64 {
        let total_chunks = self.metadata.total_size.div_ceil(self.chunk_size as u64);

        if total_chunks == 0 {
            return 0.0;
        }

        (self.chunks.len() as f64 / total_chunks as f64) * 100.0
    }
}

/// Chunk cache manager.
///
/// Manages multiple chunked entries, providing efficient lookup and storage
/// of large files split into chunks.
pub struct ChunkCache {
    entries: Arc<DashMap<String, Arc<ChunkedEntry>>>,
    default_chunk_size: usize,
    max_bytes: Option<usize>,
    max_entries: Option<usize>,
    access_seq: AtomicU64,
}

impl ChunkCache {
    /// Creates a new chunk cache with the given default chunk size and default
    /// capacity limits (128 MiB, 1,024 entries).
    ///
    /// # Arguments
    ///
    /// * `default_chunk_size` - Default size of each chunk in bytes
    ///
    /// # Examples
    ///
    /// ```rust
    /// use tower_http_cache::chunks::ChunkCache;
    ///
    /// // Create cache with 1MB chunks
    /// let cache = ChunkCache::new(1024 * 1024);
    /// ```
    pub fn new(default_chunk_size: usize) -> Self {
        Self::with_limits(default_chunk_size, Some(128 * 1024 * 1024), Some(1024))
    }

    /// Creates a new chunk cache with explicit byte and entry capacity bounds.
    pub fn with_limits(
        default_chunk_size: usize,
        max_bytes: Option<usize>,
        max_entries: Option<usize>,
    ) -> Self {
        Self {
            entries: Arc::new(DashMap::new()),
            default_chunk_size,
            max_bytes,
            max_entries,
            access_seq: AtomicU64::new(1),
        }
    }

    fn next_seq(&self) -> u64 {
        self.access_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Atomically builds a fresh [`ChunkedEntry`] populated with `chunks` and
    /// replaces any prior entry for `key`.
    ///
    /// Readers concurrently accessing `key` observe either the complete previous
    /// entry or the complete new entry, never mixed metadata and chunks.
    pub fn replace_with_ttl(
        &self,
        key: String,
        metadata: ChunkMetadata,
        ttl: Option<Duration>,
        chunks: Vec<Bytes>,
    ) -> Arc<ChunkedEntry> {
        let expires_at = ttl.map(|d| SystemTime::now() + d);
        let entry = ChunkedEntry::new_with_expiry(metadata, self.default_chunk_size, expires_at);
        let mut entry_bytes = 0usize;
        for (idx, chunk) in chunks.into_iter().enumerate() {
            entry_bytes = entry_bytes.saturating_add(chunk.len());
            entry.add_chunk(idx as u64, chunk);
        }
        entry.touch(self.next_seq());
        let arc_entry = Arc::new(entry);

        if self.max_entries == Some(0) || self.max_bytes.is_some_and(|max| entry_bytes > max) {
            self.entries.remove(&key);
            return arc_entry;
        }

        self.entries.insert(key.clone(), arc_entry.clone());
        self.evict_if_needed(Some(&key));
        arc_entry
    }

    /// Gets or creates a chunked entry.
    ///
    /// If an entry exists for the key, returns it. Otherwise, creates a new
    /// entry with the given metadata.
    ///
    /// # Arguments
    ///
    /// * `key` - Cache key
    /// * `metadata` - File metadata (used only if creating new entry)
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use tower_http_cache::chunks::{ChunkCache, ChunkMetadata};
    /// # use http::StatusCode;
    /// let cache = ChunkCache::new(1024 * 1024);
    ///
    /// let metadata = ChunkMetadata {
    ///     total_size: 10_000_000,
    ///     content_type: "video/mp4".to_string(),
    ///     etag: None,
    ///     last_modified: None,
    ///     status: StatusCode::OK,
    ///     version: http::Version::HTTP_11,
    ///     headers: vec![],
    /// };
    ///
    /// let entry = cache.get_or_create("video.mp4".to_string(), metadata);
    /// ```
    pub fn get_or_create(&self, key: String, metadata: ChunkMetadata) -> Arc<ChunkedEntry> {
        self.get_or_create_with_ttl(key, metadata, None)
    }

    /// Gets or creates an entry with an optional TTL, replacing any expired entry.
    pub fn get_or_create_with_ttl(
        &self,
        key: String,
        metadata: ChunkMetadata,
        ttl: Option<Duration>,
    ) -> Arc<ChunkedEntry> {
        let expires_at = ttl.map(|d| SystemTime::now() + d);
        let seq = self.next_seq();
        let entry = match self.entries.entry(key.clone()) {
            dashmap::mapref::entry::Entry::Occupied(mut occupied) => {
                if occupied.get().is_expired() {
                    let fresh = Arc::new(ChunkedEntry::new_with_expiry(
                        metadata,
                        self.default_chunk_size,
                        expires_at,
                    ));
                    fresh.touch(seq);
                    occupied.insert(fresh.clone());
                    fresh
                } else {
                    let existing = occupied.get().clone();
                    existing.touch(seq);
                    existing
                }
            }
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                let entry = Arc::new(ChunkedEntry::new_with_expiry(
                    metadata,
                    self.default_chunk_size,
                    expires_at,
                ));
                entry.touch(seq);
                vacant.insert(entry.clone());
                entry
            }
        };
        self.evict_if_needed(Some(&key));
        entry
    }

    fn evict_if_needed(&self, keep_key: Option<&str>) {
        // 1. Prune expired entries first.
        let expired: Vec<String> = self
            .entries
            .iter()
            .filter_map(|e| {
                if e.value().is_expired() {
                    Some(e.key().clone())
                } else {
                    None
                }
            })
            .collect();
        for k in expired {
            self.entries.remove_if(&k, |_, v| v.is_expired());
        }

        // 2. Enforce max_entries using LRU sequence.
        if let Some(max_entries) = self.max_entries {
            while self.entries.len() > max_entries {
                let victim = self
                    .entries
                    .iter()
                    .filter(|e| Some(e.key().as_str()) != keep_key)
                    .min_by_key(|e| e.value().last_accessed_seq.load(Ordering::Relaxed))
                    .map(|e| e.key().clone());
                let Some(victim_key) = victim else {
                    break;
                };
                self.entries.remove(&victim_key);
            }
        }

        // 3. Enforce max_bytes using LRU sequence.
        if let Some(max_bytes) = self.max_bytes {
            loop {
                let mut total_bytes = 0usize;
                let mut victim: Option<(String, u64)> = None;

                for entry in self.entries.iter() {
                    total_bytes = total_bytes.saturating_add(entry.value().stored_bytes());
                    if Some(entry.key().as_str()) == keep_key {
                        continue;
                    }
                    let seq = entry.value().last_accessed_seq.load(Ordering::Relaxed);
                    if victim.as_ref().is_none_or(|(_, best)| seq < *best) {
                        victim = Some((entry.key().clone(), seq));
                    }
                }

                if total_bytes <= max_bytes {
                    break;
                }
                let Some((victim_key, _)) = victim else {
                    break;
                };
                self.entries.remove(&victim_key);
            }
        }
    }

    /// Gets an entry if it exists.
    ///
    /// # Arguments
    ///
    /// * `key` - Cache key
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use tower_http_cache::chunks::{ChunkCache, ChunkMetadata};
    /// # use http::StatusCode;
    /// let cache = ChunkCache::new(1024 * 1024);
    ///
    /// assert!(cache.get("nonexistent").is_none());
    ///
    /// # let metadata = ChunkMetadata {
    /// #     total_size: 1024,
    /// #     content_type: "text/plain".to_string(),
    /// #     etag: None,
    /// #     last_modified: None,
    /// #     status: StatusCode::OK,
    /// #     version: http::Version::HTTP_11,
    /// #     headers: vec![],
    /// # };
    /// cache.get_or_create("exists".to_string(), metadata);
    /// assert!(cache.get("exists").is_some());
    /// ```
    pub fn get(&self, key: &str) -> Option<Arc<ChunkedEntry>> {
        let entry = self.entries.get(key).map(|r| r.value().clone())?;
        if entry.is_expired() {
            // Remove only if still expired to avoid clobbering a concurrent refresh.
            self.entries.remove_if(key, |_, v| v.is_expired());
            return None;
        }
        entry.touch(self.next_seq());
        Some(entry)
    }

    /// Removes an entry from the cache.
    ///
    /// # Arguments
    ///
    /// * `key` - Cache key
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use tower_http_cache::chunks::{ChunkCache, ChunkMetadata};
    /// # use http::StatusCode;
    /// let cache = ChunkCache::new(1024 * 1024);
    ///
    /// # let metadata = ChunkMetadata {
    /// #     total_size: 1024,
    /// #     content_type: "text/plain".to_string(),
    /// #     etag: None,
    /// #     last_modified: None,
    /// #     status: StatusCode::OK,
    /// #     version: http::Version::HTTP_11,
    /// #     headers: vec![],
    /// # };
    /// cache.get_or_create("temp".to_string(), metadata);
    /// assert!(cache.get("temp").is_some());
    ///
    /// cache.remove("temp");
    /// assert!(cache.get("temp").is_none());
    /// ```
    pub fn remove(&self, key: &str) -> Option<Arc<ChunkedEntry>> {
        self.entries.remove(key).map(|(_, v)| v)
    }

    /// Removes all entries from the chunk cache.
    pub fn clear(&self) {
        self.entries.clear();
    }

    /// Gets cache statistics.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use tower_http_cache::chunks::{ChunkCache, ChunkMetadata};
    /// # use bytes::Bytes;
    /// # use http::StatusCode;
    /// let cache = ChunkCache::new(1024);
    ///
    /// # let metadata = ChunkMetadata {
    /// #     total_size: 2048,
    /// #     content_type: "text/plain".to_string(),
    /// #     etag: None,
    /// #     last_modified: None,
    /// #     status: StatusCode::OK,
    /// #     version: http::Version::HTTP_11,
    /// #     headers: vec![],
    /// # };
    /// let entry = cache.get_or_create("file1".to_string(), metadata);
    /// entry.add_chunk(0, Bytes::from(vec![0u8; 1024]));
    ///
    /// let stats = cache.stats();
    /// assert_eq!(stats.total_entries, 1);
    /// assert_eq!(stats.total_chunks, 1);
    /// assert_eq!(stats.total_bytes, 1024);
    /// ```
    pub fn stats(&self) -> ChunkCacheStats {
        let mut total_entries = 0;
        let mut total_chunks = 0;
        let mut total_bytes = 0u64;
        let mut complete_entries = 0;
        let mut expired_keys = Vec::new();

        for entry in self.entries.iter() {
            let chunk_entry = entry.value();
            if chunk_entry.is_expired() {
                expired_keys.push(entry.key().clone());
                continue;
            }

            total_entries += 1;
            let chunk_count = chunk_entry.chunks.len();
            total_chunks += chunk_count;

            for chunk in chunk_entry.chunks.iter() {
                total_bytes += chunk.value().len() as u64;
            }

            if chunk_entry.is_complete() {
                complete_entries += 1;
            }
        }

        for key in expired_keys {
            self.entries.remove_if(&key, |_, v| v.is_expired());
        }

        ChunkCacheStats {
            total_entries,
            complete_entries,
            total_chunks,
            total_bytes,
        }
    }
}

/// Statistics for chunk cache.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ChunkCacheStats {
    /// Total number of cached entries
    pub total_entries: usize,

    /// Number of entries with all chunks cached
    pub complete_entries: usize,

    /// Total number of cached chunks across all entries
    pub total_chunks: usize,

    /// Total bytes stored in cache
    pub total_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_metadata(size: u64) -> ChunkMetadata {
        ChunkMetadata {
            total_size: size,
            content_type: "application/octet-stream".to_string(),
            etag: Some("test-etag".to_string()),
            last_modified: None,
            status: StatusCode::OK,
            version: Version::HTTP_11,
            headers: vec![],
        }
    }

    #[test]
    fn test_chunk_entry_new() {
        let metadata = test_metadata(1024);
        let entry = ChunkedEntry::new(metadata, 512);

        assert_eq!(entry.chunk_size, 512);
        assert_eq!(entry.metadata.total_size, 1024);
        assert_eq!(entry.chunks.len(), 0);
    }

    #[test]
    fn test_add_and_get_chunk() {
        let metadata = test_metadata(2048);
        let entry = ChunkedEntry::new(metadata, 1024);

        let chunk_data = Bytes::from(vec![1u8; 1024]);
        entry.add_chunk(0, chunk_data.clone());

        let retrieved = entry.get_chunk(0);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap(), chunk_data);
    }

    #[test]
    fn test_get_nonexistent_chunk() {
        let metadata = test_metadata(2048);
        let entry = ChunkedEntry::new(metadata, 1024);

        assert!(entry.get_chunk(0).is_none());
        assert!(entry.get_chunk(999).is_none());
    }

    #[test]
    fn test_get_range_single_chunk() {
        let metadata = test_metadata(1024);
        let entry = ChunkedEntry::new(metadata, 1024);

        let chunk_data = Bytes::from((0..1024).map(|i| i as u8).collect::<Vec<u8>>());
        entry.add_chunk(0, chunk_data);

        // Get first 512 bytes
        let range = entry.get_range(0, 511);
        assert!(range.is_some());
        let data = range.unwrap();
        assert_eq!(data.len(), 512);
        assert_eq!(data[0], 0);
        assert_eq!(data[511], 255);
    }

    #[test]
    fn test_get_range_multiple_chunks() {
        let metadata = test_metadata(3072);
        let entry = ChunkedEntry::new(metadata, 1024);

        // Add 3 chunks
        entry.add_chunk(0, Bytes::from(vec![0u8; 1024]));
        entry.add_chunk(1, Bytes::from(vec![1u8; 1024]));
        entry.add_chunk(2, Bytes::from(vec![2u8; 1024]));

        // Get range spanning chunks 0 and 1
        let range = entry.get_range(512, 1535);
        assert!(range.is_some());
        let data = range.unwrap();
        assert_eq!(data.len(), 1024);
        assert_eq!(data[0], 0); // From chunk 0
        assert_eq!(data[512], 1); // From chunk 1
    }

    #[test]
    fn test_get_range_missing_chunk() {
        let metadata = test_metadata(2048);
        let entry = ChunkedEntry::new(metadata, 1024);

        entry.add_chunk(0, Bytes::from(vec![0u8; 1024]));
        // Chunk 1 is missing

        // Try to get range spanning both chunks
        let range = entry.get_range(512, 1535);
        assert!(range.is_none()); // Should fail because chunk 1 is missing
    }

    #[test]
    fn test_is_complete_empty() {
        let metadata = test_metadata(2048);
        let entry = ChunkedEntry::new(metadata, 1024);

        assert!(!entry.is_complete());
    }

    #[test]
    fn test_is_complete_partial() {
        let metadata = test_metadata(2048);
        let entry = ChunkedEntry::new(metadata, 1024);

        entry.add_chunk(0, Bytes::from(vec![0u8; 1024]));
        assert!(!entry.is_complete());
    }

    #[test]
    fn test_is_complete_full() {
        let metadata = test_metadata(2048);
        let entry = ChunkedEntry::new(metadata, 1024);

        entry.add_chunk(0, Bytes::from(vec![0u8; 1024]));
        entry.add_chunk(1, Bytes::from(vec![1u8; 1024]));
        assert!(entry.is_complete());
    }

    #[test]
    fn test_coverage() {
        let metadata = test_metadata(4096);
        let entry = ChunkedEntry::new(metadata, 1024);

        assert_eq!(entry.coverage(), 0.0);

        entry.add_chunk(0, Bytes::from(vec![0u8; 1024]));
        assert_eq!(entry.coverage(), 25.0);

        entry.add_chunk(1, Bytes::from(vec![1u8; 1024]));
        assert_eq!(entry.coverage(), 50.0);

        entry.add_chunk(2, Bytes::from(vec![2u8; 1024]));
        assert_eq!(entry.coverage(), 75.0);

        entry.add_chunk(3, Bytes::from(vec![3u8; 1024]));
        assert_eq!(entry.coverage(), 100.0);
    }

    #[test]
    fn test_chunk_cache_new() {
        let cache = ChunkCache::new(1024 * 1024);
        let stats = cache.stats();

        assert_eq!(stats.total_entries, 0);
        assert_eq!(stats.total_chunks, 0);
        assert_eq!(stats.total_bytes, 0);
    }

    #[test]
    fn test_chunk_cache_get_or_create() {
        let cache = ChunkCache::new(1024);
        let metadata = test_metadata(2048);

        let entry1 = cache.get_or_create("key1".to_string(), metadata.clone());
        let entry2 = cache.get_or_create("key1".to_string(), test_metadata(4096));

        // Should return the same entry (original metadata)
        assert_eq!(entry1.metadata.total_size, entry2.metadata.total_size);
        assert_eq!(entry1.metadata.total_size, 2048);
    }

    #[test]
    fn test_chunk_cache_get() {
        let cache = ChunkCache::new(1024);

        assert!(cache.get("nonexistent").is_none());

        let metadata = test_metadata(1024);
        cache.get_or_create("exists".to_string(), metadata);

        assert!(cache.get("exists").is_some());
    }

    #[test]
    fn test_chunk_cache_remove() {
        let cache = ChunkCache::new(1024);
        let metadata = test_metadata(1024);

        cache.get_or_create("temp".to_string(), metadata);
        assert!(cache.get("temp").is_some());

        let removed = cache.remove("temp");
        assert!(removed.is_some());
        assert!(cache.get("temp").is_none());
    }

    #[test]
    fn test_chunk_cache_stats() {
        let cache = ChunkCache::new(1024);

        let metadata1 = test_metadata(2048);
        let entry1 = cache.get_or_create("file1".to_string(), metadata1);
        entry1.add_chunk(0, Bytes::from(vec![0u8; 1024]));
        entry1.add_chunk(1, Bytes::from(vec![1u8; 1024]));

        let metadata2 = test_metadata(3072);
        let entry2 = cache.get_or_create("file2".to_string(), metadata2);
        entry2.add_chunk(0, Bytes::from(vec![2u8; 1024]));

        let stats = cache.stats();
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.complete_entries, 1); // Only entry1 is complete
        assert_eq!(stats.total_chunks, 3);
        assert_eq!(stats.total_bytes, 3072);
    }

    #[test]
    fn test_chunk_cache_ttl_expiry_and_pruning() {
        let cache = ChunkCache::new(1024);
        let entry = cache.get_or_create_with_ttl(
            "short-lived".to_string(),
            test_metadata(1024),
            Some(Duration::from_millis(15)),
        );
        entry.add_chunk(0, Bytes::from(vec![7u8; 1024]));
        assert!(cache.get("short-lived").is_some());

        std::thread::sleep(Duration::from_millis(30));

        // Stats and get should prune the expired entry cleanly.
        let stats = cache.stats();
        assert_eq!(stats.total_entries, 0);
        assert_eq!(stats.total_bytes, 0);
        assert!(cache.get("short-lived").is_none());

        // Creating again with TTL replaces expired entry with fresh metadata.
        let renewed = cache.get_or_create_with_ttl(
            "short-lived".to_string(),
            test_metadata(2048),
            Some(Duration::from_secs(60)),
        );
        assert_eq!(renewed.metadata.total_size, 2048);
        assert!(cache.get("short-lived").is_some());
    }

    #[test]
    fn test_chunk_cache_replace_with_ttl_replaces_live_entry() {
        let cache = ChunkCache::new(8);
        cache.replace_with_ttl(
            "file".to_string(),
            test_metadata(16),
            Some(Duration::from_secs(60)),
            vec![
                Bytes::from_static(b"AAAAAAAA"),
                Bytes::from_static(b"BBBBBBBB"),
            ],
        );

        // Replace live entry with a smaller body and new metadata.
        let mut v2_meta = test_metadata(8);
        v2_meta.etag = Some("\"v2\"".to_string());
        cache.replace_with_ttl(
            "file".to_string(),
            v2_meta,
            Some(Duration::from_secs(60)),
            vec![Bytes::from_static(b"zzzzzzzz")],
        );

        let entry = cache.get("file").expect("entry present");
        assert_eq!(entry.metadata.total_size, 8);
        assert_eq!(entry.metadata.etag.as_deref(), Some("\"v2\""));
        assert_eq!(entry.chunks.len(), 1);
        assert_eq!(
            entry.get_range(0, 7).unwrap(),
            Bytes::from_static(b"zzzzzzzz")
        );
    }

    #[test]
    fn test_chunk_cache_enforces_max_entries_and_max_bytes() {
        // Max 2 entries, max 16 bytes
        let cache = ChunkCache::with_limits(8, Some(16), Some(2));

        cache.replace_with_ttl(
            "k1".to_string(),
            test_metadata(8),
            None,
            vec![Bytes::from_static(b"11111111")],
        );
        cache.replace_with_ttl(
            "k2".to_string(),
            test_metadata(8),
            None,
            vec![Bytes::from_static(b"22222222")],
        );
        // Touch k1 so k2 becomes LRU
        assert!(cache.get("k1").is_some());

        cache.replace_with_ttl(
            "k3".to_string(),
            test_metadata(8),
            None,
            vec![Bytes::from_static(b"33333333")],
        );
        let stats = cache.stats();
        assert_eq!(stats.total_entries, 2);
        assert!(stats.total_bytes <= 16);
        assert!(cache.get("k1").is_some());
        assert!(cache.get("k2").is_none());
        assert!(cache.get("k3").is_some());

        // Single entry larger than max_bytes is not stored
        cache.replace_with_ttl(
            "too-big".to_string(),
            test_metadata(24),
            None,
            vec![Bytes::from(vec![b'x'; 24])],
        );
        assert!(cache.get("too-big").is_none());
    }
}
