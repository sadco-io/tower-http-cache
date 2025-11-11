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
//! ```rust
//! use tower_http_cache::chunks::{ChunkCache, ChunkMetadata};
//! use bytes::Bytes;
//! use http::StatusCode;
//!
//! # tokio_test::block_on(async {
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
//! # });
//! ```

use bytes::Bytes;
use dashmap::DashMap;
use http::{StatusCode, Version};
use std::sync::Arc;

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
        Self {
            metadata,
            chunks: Arc::new(DashMap::new()),
            chunk_size,
        }
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
}

impl ChunkCache {
    /// Creates a new chunk cache with the given default chunk size.
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
        Self {
            entries: Arc::new(DashMap::new()),
            default_chunk_size,
        }
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
        self.entries
            .entry(key)
            .or_insert_with(|| Arc::new(ChunkedEntry::new(metadata, self.default_chunk_size)))
            .clone()
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
        self.entries.get(key).map(|r| r.value().clone())
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

        for entry in self.entries.iter() {
            total_entries += 1;
            let chunk_entry = entry.value();
            let chunk_count = chunk_entry.chunks.len();
            total_chunks += chunk_count;

            for chunk in chunk_entry.chunks.iter() {
                total_bytes += chunk.value().len() as u64;
            }

            if chunk_entry.is_complete() {
                complete_entries += 1;
            }
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
}
