//! Re-exports for consumers who prefer a single import.
//!
//! ```no_run
//! use tower_http_cache::prelude::*;
//! # use std::time::Duration;
//! # let backend = InMemoryBackend::new(128);
//! let layer = CacheLayer::builder(backend)
//!     .ttl(Duration::from_secs(30))
//!     .build();
//! ```

#[cfg(feature = "memcached-backend")]
pub use crate::backend::memcached::{MemcachedBackend, MemcachedBackendBuilder, PoolState};
pub use crate::backend::memory::InMemoryBackend;
pub use crate::backend::multi_tier::MultiTierBackend;
#[cfg(feature = "redis-backend")]
pub use crate::backend::redis::RedisBackend;
pub use crate::backend::{CacheBackend, CacheEntry};
pub use crate::chunks::{ChunkCache, ChunkCacheStats, ChunkMetadata, ChunkedEntry};
pub use crate::codec::{BincodeCodec, CacheCodec};
pub use crate::layer::{CacheLayer, CacheLayerBuilder, KeyExtractor};
pub use crate::logging::{CacheEvent, CacheEventType, MLLoggingConfig};
pub use crate::policy::{CachePolicy, CompressionConfig, CompressionStrategy};
pub use crate::range::{RangeHandling, RangeRequest};
pub use crate::streaming::{StreamingDecision, StreamingPolicy};
pub use crate::tags::{TagIndex, TagPolicy};
