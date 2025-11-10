# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0] - 2025-11-10

### Added

#### Smart Streaming & Large File Handling
- **Intelligent body size detection**: Prevent large files from overwhelming cache
  - `StreamingPolicy` for configurable streaming behavior
  - Early detection via `Content-Length` header and `size_hint()`
  - Content-Type based filtering (PDFs, videos, archives excluded by default)
  - Configurable `max_cacheable_size` (default: 1MB)
  - Wildcard content-type matching (e.g., `video/*`, `audio/*`)
  - Force-cache lists for critical API responses
  - Multi-tier size protection (large entries excluded from L1)
  - 14 comprehensive integration tests
  - 20+ unit tests with 100% branch coverage

#### Multi-Tier Size Protection
- **max_l1_entry_size**: Prevent large entries from polluting fast L1 cache
  - Configurable size limit for L1 promotion (default: 256KB)
  - Automatic size checking during write-through and promotion
  - Large entries stored only in L2 for capacity efficiency
  - Metrics tracking for skipped L1 writes and promotions
  - Zero performance impact on small entries

### Changed
- Streaming policy enabled by default (can be disabled)
- Size limits now apply consistently to all content types (including forced-cache types)

### Performance
- Eliminates memory exhaustion from large file responses
- Prevents cache pollution from 5-20MB files
- Protects L1 cache from unnecessary large entry storage
- < 1% overhead on streaming decision path

## [0.3.0] - 2025-11-10

### Added

#### Cache Tags & Invalidation Groups
- **Tag-based cache invalidation**: Group related cache entries with tags and invalidate them together
  - `TagPolicy` for configuring tag behavior
  - `TagIndex` for efficient bidirectional tag→key and key→tag lookups
  - `invalidate_by_tag()` and `invalidate_by_tags()` methods
  - Automatic cleanup of orphaned tag entries
  - Thread-safe using `DashMap` for lock-free concurrent access
  - Integrated with both in-memory and Redis backends
  - 17 comprehensive unit tests

#### Multi-Tier Caching
- **L1 + L2 hybrid backend**: Combine fast in-memory cache with larger distributed storage
  - `MultiTierBackend<L1, L2>` generic over any two `CacheBackend` implementations
  - Automatic promotion from L2→L1 based on access patterns
  - Configurable `PromotionStrategy` (HitCount, HitRate)
  - Per-key access tracking with atomic operations
  - Write-through and write-back modes
  - Graceful tier failure handling
  - Tier-specific metrics and observability
  - < 2% performance overhead
  - 7 integration tests

#### ML-Ready Structured Logging
- **Request correlation and ML training data**: Comprehensive structured logging for analytics
  - `RequestId` type for request correlation (following X-Request-ID header)
  - `MLLoggingConfig` for configurable sampling, key hashing, and privacy controls
  - Rich JSON event format with 15+ metadata fields
  - SHA-256 key hashing option for privacy compliance
  - Integration with `tracing` crate for structured output
  - Cost and complexity tracking for ML model training
  - Configurable sampling rate to reduce overhead
  - 15 unit tests

#### Admin API & Observability
- **REST API for cache introspection**: Production-ready management endpoints
  - 7 REST endpoints for cache management:
    - `GET /health` - Health check
    - `GET /stats` - Overall statistics
    - `GET /hot-keys` - Most accessed keys
    - `GET /tags` - List all tags
    - `POST /invalidate` - Invalidate by key or tag
    - `GET /keys` - List cached keys (planned)
    - `GET /key/:key` - Inspect specific key (planned)
  - Token-based authentication (Bearer token)
  - Real-time statistics collection
  - Hot keys tracking with configurable limits
  - JSON response format for all endpoints
  - Optional feature flag: `admin-api`
  - 19 unit tests

### Changed

- Enhanced `CachePolicy` with `tag_policy`, `ml_logging`, and `tag_extractor` fields
- Updated `CacheEntry` to include optional `tags` field
- Extended `CacheBackend` trait with default implementations for tag operations
- Integrated tag support into `InMemoryBackend`

### Dependencies

- Added `uuid` 1.0 with v4 and serde features
- Added `sha2` 0.10 for key hashing
- Added `hex` 0.4 for hash encoding
- Added `chrono` 0.4 for timestamp handling
- Added optional `axum` 0.8 for admin API (behind `admin-api` feature)
- Added optional `governor` 0.6 for rate limiting (behind `admin-api` feature)

### Performance

- Tag indexing: < 1% overhead on cache set operations
- Multi-tier: < 2% total overhead (L1 hot path unchanged)
- ML logging: < 100µs per event with sampling
- Request ID extraction: Negligible (simple header lookup)

### Non-Breaking Changes

All v0.3.0 features are opt-in and backward compatible:
- Default behavior unchanged
- No breaking API changes
- All features disabled by default and require explicit configuration

## [0.2.0] - 2025-11-10

### Added

- **Auto-refresh functionality**: Proactively refreshes frequently-accessed cache entries before they expire
  - Lock-free frequency tracking using `AtomicU64` and `DashMap` for minimal performance overhead (< 1%)
  - Configurable hit rate thresholds with sliding time windows
  - Background task management with graceful shutdown via `Drop`
  - Concurrency control using semaphore-based limits
  - Request reconstruction from stored metadata
  - Full observability support with metrics and tracing
  - Comprehensive test coverage with 22 new tests
  - `AutoRefreshConfig` for fine-grained configuration
  - `init_auto_refresh()` method to enable proactive cache warming
- Added tokio features: `rt`, `time`, `macros` for background task support

### Changed

- Enhanced `CacheLayer` with auto-refresh capabilities
- Enhanced `CacheService` to track cache hits for frequency analysis
- Non-breaking change: auto-refresh is disabled by default and requires explicit configuration

## [0.1.2] - 2025-11-09

### Fixed

- Added `Clone` implementation to `CacheService` to resolve compatibility issues with Axum's `Router::layer` API

## [0.1.1] - 2025-11-09

### Fixed

- Corrected repository URL in Cargo.toml to point to `sadco-io/tower-http-cache`

## [0.1.0] - 2025-11-09

### Added

- Initial release of `tower-http-cache`
- Drop-in `CacheLayer` for Tower services
- Stampede protection with request deduplication
- Flexible TTL configuration (positive/negative TTL, refresh-before-expiry)
- Stale-while-revalidate support
- Pluggable storage backends:
  - In-memory backend powered by Moka
  - Redis backend with async pooling (optional `redis-backend` feature)
- Policy controls:
  - Min/max body size limits
  - Cache-Control header respect/override
  - Custom method and status code filters
  - Header allowlisting
- Custom cache key extraction
- Optional observability:
  - Metrics counters via `metrics` crate (optional `metrics` feature)
  - Tracing spans (optional `tracing` feature)
- Optional gzip compression (optional `compression` feature)
- Comprehensive test suite
- Benchmark suite with Criterion
- Examples for Axum and Redis integration

[Unreleased]: https://github.com/sadco-io/tower-http-cache/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/sadco-io/tower-http-cache/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/sadco-io/tower-http-cache/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/sadco-io/tower-http-cache/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/sadco-io/tower-http-cache/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/sadco-io/tower-http-cache/releases/tag/v0.1.0
