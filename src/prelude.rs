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
#[cfg(feature = "in-memory")]
pub use crate::backend::memory::InMemoryBackend;
pub use crate::backend::multi_tier::MultiTierBackend;
#[cfg(feature = "redis-backend")]
pub use crate::backend::redis::RedisBackend;
pub use crate::backend::{CacheBackend, CacheEntry};
pub use crate::chunks::{ChunkCache, ChunkCacheStats, ChunkMetadata, ChunkedEntry};
#[cfg(feature = "serde")]
#[expect(
    deprecated,
    reason = "BincodeCodec is a deprecated alias kept through 0.6.x"
)]
pub use crate::codec::BincodeCodec;
#[cfg(feature = "serde")]
pub use crate::codec::{CacheCodec, PostcardCodec};
pub use crate::layer::{CacheLayer, CacheLayerBuilder, KeyExtractor};
#[cfg(feature = "serde")]
pub use crate::logging::CacheEvent;
pub use crate::logging::{CacheEventType, MLLoggingConfig};
pub use crate::policy::{CachePolicy, CompressionConfig, CompressionStrategy};
pub use crate::range::{RangeHandling, RangeRequest};
pub use crate::streaming::{StreamingDecision, StreamingPolicy};
pub use crate::tags::{TagIndex, TagPolicy};
