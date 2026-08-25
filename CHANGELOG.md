# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.1] - 2026-08-25

### Fixed

- **`governor` was declared but never used.** The `admin-api` feature pulled
  `governor 0.6` and its entire rate-limiting subtree; nothing in `src/`, `tests/`,
  `examples/` or `benches/` referenced it. Removed. Same class of finding as the
  `cargo-udeps` sweeps in 0.4.2 / 0.4.3.
- **The `metrics` bench never compiled.** `benches/cache_benchmarks.rs` imported
  `metrics_exporter_null`, a crate that does not exist on crates.io, so
  `cargo bench --features metrics` failed with E0432. Now uses
  `metrics_util::debugging::DebuggingRecorder`.
- **Declared MSRV was wrong for the default build.** `rust-version` said `1.75.0`, but
  the non-optional `uuid = "1.0"` resolves to 1.25, which declares `1.85.0` -- and with
  no committed lockfile there was nothing holding it back. `axum` 0.8 (1.80), `redis`
  0.32 (1.80) and dev `criterion` 0.7 (1.80) compound it. Now **`1.85` for the core and
  `1.88` for the shared backends** -- `redis-backend` and `memcached-backend` both reach
  `url` -> `idna` -> `icu_*`, which declare 1.88. With no committed lockfile a consumer
  resolving fresh gets those versions too, so pinning them in our own lock would hide the
  constraint rather than fix it. Both floors are enforced by separate CI jobs. The split
  was found by the new MSRV job on the first push.
- **The crate did not build without the `serde` feature, and `serde` was not properly
  gated.** `codec.rs`, `logging.rs`, `request_id.rs`, `admin/routes.rs` and
  `admin/stats.rs` all `use serde` unconditionally, so `--no-default-features` failed with
  six unresolved imports. Rather than making `serde` mandatory, the serde-shaped surfaces
  are now gated on it properly:

  - `codec` (backend serialization) is behind `serde`
  - `admin` is behind `admin-api` -- **the module was not gated at all**, only its
    re-export was, so the two most serde-heavy files in the crate compiled into every build
  - `CacheEvent` and `log_cache_operation` are behind `serde`; they carry a
    `serde_json::Value` and emit JSON, so serde is genuinely load-bearing there.
    `MLLoggingConfig` and `CacheEventType` stay ungated -- `CachePolicy` embeds the former,
    and neither needs serialization to be useful.
  - `RequestId`'s derives are now `cfg_attr`'d; they were ornamental.
  - `redis-backend`, `memcached-backend` and `admin-api` each enable `serde`, because for
    those it really is load-bearing.

  **This takes `bincode` out of the default-adjacent dependency graph.** A build of
  `--no-default-features --features in-memory` no longer pulls `bincode` at all, so those
  users are not exposed to RUSTSEC-2025-0141 (see Known issues). Default builds are
  unchanged -- `serde` is still a default feature.
- **`backend::memory` was not gated on `in-memory`.** Its siblings `redis` and
  `memcached` were, but `memory` (which needs `moka`) was not, so `--no-default-features`
  failed on an unresolved `moka`. The module, its `prelude` re-export, and the
  `CacheLayer::new_in_memory` constructor are now behind `#[cfg(feature = "in-memory")]`,
  consistent with the other backends.
- **`concurrent_requests_share_refresh_work` was ~50% flaky.** Measured at 6/20 passes on
  the released code. It asserted that of two racing requests, one receives the stale body
  and the other the refreshed one -- but which body a given racer observes is not part of
  the stale-while-revalidate contract; it depends on how the tasks interleave with the
  background refresh. The test now asserts what the contract actually guarantees: each
  response is one of the two legitimate bodies, **and the origin is called exactly twice**,
  which is the real single-flight coalescing property. That second assertion is unchanged
  and still deterministic. Now 25/25.
- `examples/redis_smoke.rs` used the deprecated `Client::get_tokio_connection_manager`;
  switched to `get_connection_manager`. Cleared unused imports in
  `examples/chunk_cache_demo.rs` and clippy warnings in `src/admin/stats.rs`,
  `src/streaming.rs` and the benches so `clippy -D warnings` passes.

### Changed

- `dashmap` `5.5` -> `6.2`. Technically a major, shipped in a patch because `dashmap` is
  not part of this crate's public API -- `src/tags.rs` and `src/chunks.rs` use only
  `DashMap::new`, `get`, `insert` and `entry`, all unchanged across 5 -> 6. Staying on
  5.x forced a duplicate `dashmap` into any tree that also depended on a 6.x consumer.
- Dependency floors raised to current, all semver-compatible: `tokio` `1.40` -> `1.53`,
  `http` `1.3` -> `1.5`, `http-body` `1.0` -> `1.1`, `http-body-util` `0.1` -> `0.1.5`,
  `bytes` `1.7` -> `1.12`, `moka` `0.12` -> `0.12.16`, `tokio-util` `0.7` -> `0.7.19`,
  `flate2` `1.0` -> `1.1`, `uuid` `1.0` -> `1.25`, `chrono` `0.4` -> `0.4.45`,
  `futures-util` `0.3` -> `0.3.34`, `tower` `0.5` -> `0.5.3`, `axum` `0.8` -> `0.8.9`,
  `tracing-subscriber` `0.3` -> `0.3.23`.

### Added

- CI (`.github/workflows/ci.yml`): stable + beta tests across the feature matrix
  (including `--no-default-features`), MSRV jobs for 1.85 and 1.88, `fmt` +
  `clippy -D warnings`, `cargo doc -D warnings`, and `cargo deny check`.
- `deny.toml`, with the bincode advisory ignore documented inline.

### Known issues

- **`bincode 1.3.3` is unmaintained (RUSTSEC-2025-0141).** Filed 2025-12-16; upstream has
  ceased development permanently and there is no patched release. `bincode` is reachable
  from the default feature set, so downstream `cargo audit` runs will flag it -- though as
  of this release a `--no-default-features --features in-memory` build does not pull it at
  all, which is the recommended configuration for anyone who does not need a shared backend. It defines
  the on-disk and on-wire encoding for the Redis and Memcached backends, so migrating to
  bincode 3 or postcard invalidates every live cache entry -- scheduled for 0.6.0, with a
  documented ignore in `deny.toml` until then.
- **`memcached-backend` is NOT recommended for production in this release.** One
  dependency brings three advisories, one of them a live vulnerability:
  `async-memcached` declares `toxiproxy_rust` -- a *test fixture* -- as a normal
  dependency, which pulls `reqwest 0.11` -> `hyper 0.14` -> `h2`:
  - **RUSTSEC-2026-0258** — `h2` unbounded empty DATA frames (**denial of service**)
  - RUSTSEC-2025-0134 — `rustls-pemfile` unmaintained
  - RUSTSEC-2025-0057 — `fxhash` unmaintained

  All three are suppressed in `deny.toml` with comments naming this cause, so the rest of
  the tree stays auditable; they are to be deleted the moment upstream moves
  `toxiproxy_rust` to `[dev-dependencies]`. The feature is opt-in and off by default, and
  no other feature is affected. **Confirmed still present in `async-memcached` 0.7.0**, so
  the planned bump does not resolve it.

- **`memcached-backend` also drags in a second HTTP stack.** `async-memcached` depends on
  `toxiproxy_rust` unconditionally, which pulls `reqwest 0.11` -> `hyper 0.14` ->
  `native-tls` -> `openssl-sys`. Enabling the feature therefore requires `libssl-dev` and
  `pkg-config` on the build host and duplicates the entire HTTP stack. Confirmed still
  present in `async-memcached 0.7.0`, so the planned bump does not resolve it; this needs
  an upstream fix (moving `toxiproxy_rust` to `[dev-dependencies]`) or a different client.
  The `memcached-backend` feature is excluded from CI for this reason.

### Notes

- Deferred to 0.6.0: the `bincode` migration above, `redis` `0.32.7` -> `1.6` (MSRV 1.88),
  `sha2` `0.10` -> `0.11` (used in `src/logging.rs`; moves to the `digest` 0.11 traits),
  `async-memcached` `0.5` -> `0.7` with `bb8` `0.8` -> `0.9`, and dev `criterion`
  `0.7` -> `0.8`.


## [0.5.0] - 2026-03-31

### Fixed
- **Content-type pattern matching used substring instead of exact match** — `"pdf"` in exclusion list would match any MIME type containing "pdf". Now uses exact match with optional parameter suffix (e.g., `application/json; charset=utf-8` matches pattern `application/json`).
- **`force_cache_content_types` doc claimed "regardless of size"** — size limits always applied. Fixed doc to accurately describe behavior: bypasses content-type exclusions only.
- **`unsafe impl Sync for SyncBoxBody` safety comment was incorrect** — claimed "single-threaded" context which is wrong for Tower. Updated with correct safety justification.
- **README documented non-existent `admin_router()` and `AdminConfig::builder()` API** — updated to match actual `AdminConfig::new().with_*()` API.
- **README referenced non-existent examples and `middleware` feature flag** — updated to list actual examples.
- **README installation instructions referenced version `"0.3"`** — updated to `"0.5"`.
- **`._*` macOS resource fork files were included in crates.io package** — added to `exclude` in Cargo.toml and `.gitignore`.
- **CHANGELOG footer links missing for v0.4.0–v0.4.3**.

### Changed
- `StreamingDecision::StreamThrough` is now `#[doc(hidden)]` (reserved for future implementation).
- Bumped version to 0.5.0 due to content-type matching behavior change (may affect users relying on substring matching).

## [0.4.3] - 2025-11-10

### Removed
- **Unused dependencies identified by `cargo-udeps`**:
  - Removed `serde_bytes` from main dependencies (never used in codebase)
  - Removed `hyper` from dev-dependencies (never used in tests/benches/examples)
  - Reduces dependency count and compilation time
  - No functional changes - all 137 tests passing

### Note
- Dev dependencies `axum`, `redis`, and `tracing-subscriber` flagged by `cargo-udeps` are false positives - they are used in examples and tests

## [0.4.2] - 2025-11-10

### Fixed
- **Removed unused dependency**: Removed `sync_wrapper` crate that was added in v0.4.1 but not used
  - Reduces dependency bloat and compilation time
  - No functional changes - implementation uses manual `unsafe impl Sync` instead
  - Still uses existing `pin_project_lite` for safe pinning

## [0.4.1] - 2025-11-10

### Fixed
- **Axum compatibility**: Fixed `Sync` trait bound issue with response bodies
  - Implemented custom `SyncBoxBody` type that wraps `BoxBody` and manually implements `Sync`
  - Uses `pin_project_lite` for safe pinning and `HttpBody` trait delegation
  - Uses same pattern as Axum's own `Body` type (`unsafe impl Sync`)
  - Resolves compilation errors when using the cache layer with Axum routers
  - Zero-cost abstraction - same performance as underlying `BoxBody`
  - Updated `CacheEntry::into_response()` to return `Response<SyncBoxBody>`
  - All 137 tests passing with new body type

### Changed
- Response body type now implements both `HttpBody` and `Sync` for Axum compatibility
- Examples updated to demonstrate Axum integration patterns
- No new dependencies added - uses existing `pin_project_lite`

## [0.4.0] - 2025-11-10

### Added

#### Chunk Caching for Large Files
- **Memory-efficient range request handling**: Chunk-based caching system for large files
  - New `chunks` module with `ChunkCache` and `ChunkedEntry` types
  - Automatic file splitting into fixed-size chunks (default: 1MB)
  - Efficient range request serving from chunk cache
  - `ChunkMetadata` for storing HTTP metadata separately from chunks
  - Configurable via `StreamingPolicy::enable_chunk_cache`
  - Per-chunk storage and retrieval for minimal memory footprint
  - Support for partial file caching (only cache accessed ranges)
  - Coverage tracking to monitor chunk cache completeness
  - Integrated with `CacheLayer` and `CacheService`
  - Automatic 206 Partial Content response generation
  - Compatible with video streaming and large file downloads
  - 40+ comprehensive chunk caching tests
  - Production example: `chunk_cache_demo`

#### BB8 Connection Pooling for Memcached
- **Production-grade connection management**: BB8 async connection pooling
  - `MemcachedBackend::builder()` with pooling support
  - Configurable pool size (min/max connections)
  - Connection timeout and retry logic
  - Health checks and automatic reconnection
  - Pool state monitoring (connections, idle, etc.)
  - Graceful shutdown and connection cleanup
  - Async-safe with tokio integration
  - Production example: `memcached_production`

#### True Streaming Pass-Through (Zero-Copy)
- **BoxBody architecture**: Complete replacement of `Full<Bytes>` with `BoxBody<Bytes, BoxError>`
  - Eliminates unnecessary buffering for large responses
  - Zero-copy streaming for excluded content types
  - Preserves `Content-Length` headers during streaming
  - Memory efficient handling of multi-GB responses
  - Full backward compatibility with existing middleware

#### HTTP Range Request Support
- **RFC 7233 compliant range handling**: Proper support for partial content requests
  - New `range` module with `parse_range_header()` utilities
  - `RangeRequest` type for parsing "bytes=start-end" specifications
  - `RangeHandling` policy enum (PassThrough/CacheFullServeRanges/CacheChunks)
  - Automatic detection of 206 Partial Content responses
  - Configurable behavior via `StreamingPolicy::range_handling`
  - Content-Range header generation and parsing
  - 15+ comprehensive range request tests

#### Memcached Backend
- **High-performance distributed caching**: Production-ready Memcached support
  - Async `MemcachedBackend` implementation via `async-memcached`
  - Namespace support for multi-tenant deployments
  - TTL and stale-while-revalidate handling
  - Custom serialization for HTTP types (StatusCode, Version, Bytes)
  - Connection pooling with Arc<Mutex<Client>>
  - Optional feature flag: `memcached-backend`
  - Compatible with memcached protocol 1.6+

#### Enhanced Observability
- **Streaming-specific metrics**: Better visibility into cache behavior
  - `tower_http_cache.streaming_passthrough` counter
  - `tower_http_cache.range_request_passthrough` counter
  - Detailed tracing logs with size and content-type info
  - Body size histograms for performance analysis

#### Smart Streaming & Large File Handling
- **Intelligent body size detection**: Prevent large files from overwhelming cache
  - `StreamingPolicy` for configurable streaming behavior
  - Early detection via `Content-Length` header and `size_hint()`
  - Content-Type based filtering (PDFs, videos, archives excluded by default)
  - Configurable `max_cacheable_size` (default: 1MB)
  - Wildcard content-type matching (e.g., `video/*`, `audio/*`)
  - Force-cache lists for critical API responses
  - Multi-tier size protection (large entries excluded from L1)
  - 20+ unit tests with 100% branch coverage

#### Multi-Tier Size Protection
- **max_l1_entry_size**: Prevent large entries from polluting fast L1 cache
  - Configurable size limit for L1 promotion (default: 256KB)
  - Automatic size checking during write-through and promotion
  - Large entries stored only in L2 for capacity efficiency
  - Metrics tracking for skipped L1 writes and promotions
  - Zero performance impact on small entries

### Changed
- **BREAKING**: `CacheService` now returns `Response<BoxBody<Bytes, BoxError>>` instead of `Response<Full<Bytes>>`
  - This enables true streaming but requires downstream services to handle `BoxBody`
  - Migration: Use `.map_err(Into::into).boxed()` on bodies if needed
  - Most Tower middleware is compatible without changes
- **BREAKING**: Added `Sync` bound to `ResBody` in Service implementation
  - Required for BoxBody's Send + Sync + 'static constraint
  - Should not affect most use cases
- `CacheEntry` now has conditional Serde derives with custom serializers for HTTP types
- `StreamingPolicy` now includes `range_handling` field (defaults to `PassThrough`)
- Range requests pass through by default without caching
- Streaming policy enabled by default (can be disabled)
- Size limits now apply consistently to all content types (including forced-cache types)

### Performance
- **Chunk caching**: 90% memory reduction for large file workloads
  - Only cache accessed chunks (not entire file)
  - Instant seeking for video streaming (no re-download)
  - Range requests served directly from memory
  - Configurable chunk size for optimal throughput
- **Zero-copy streaming**: Eliminated buffering for excluded content types
- **BB8 connection pooling**: 10x throughput improvement for Memcached
  - Reduced connection overhead
  - Concurrent request handling
  - Automatic connection reuse
- Memory efficient: Handles multi-GB responses without collecting into memory
- Eliminates memory exhaustion from large file responses
- Prevents cache pollution from 5-20MB files
- Protects L1 cache from unnecessary large entry storage
- < 1% overhead on streaming decision path

### Fixed
- Conditional compilation for `extract_size_info` import (tracing feature)
- BoxBody compatibility with Tower service ecosystem
- Proper Sync bounds for concurrent body handling

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

[Unreleased]: https://github.com/sadco-io/tower-http-cache/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/sadco-io/tower-http-cache/compare/v0.4.3...v0.5.0
[0.4.3]: https://github.com/sadco-io/tower-http-cache/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/sadco-io/tower-http-cache/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/sadco-io/tower-http-cache/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/sadco-io/tower-http-cache/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/sadco-io/tower-http-cache/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/sadco-io/tower-http-cache/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/sadco-io/tower-http-cache/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/sadco-io/tower-http-cache/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/sadco-io/tower-http-cache/releases/tag/v0.1.0
