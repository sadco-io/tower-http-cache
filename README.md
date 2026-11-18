# tower-http-cache

[![Crates.io](https://img.shields.io/crates/v/tower-http-cache)](https://crates.io/crates/tower-http-cache)
[![Documentation](https://docs.rs/tower-http-cache/badge.svg)](https://docs.rs/tower-http-cache)
[![License](https://img.shields.io/github/license/sadco-io/tower-http-cache)](https://github.com/sadco-io/tower-http-cache/blob/master/LICENSE-MIT)

Tower middleware for HTTP response caching with pluggable storage backends (in-memory, Redis, and more). `tower-http-cache` brings a production-grade caching layer to Tower/Axum/Hyper stacks, with stampede protection, stale-while-revalidate, header allowlisting, compression, and policy controls out of the box.

---

## Features at a Glance

- ✅ **Drop-in `CacheLayer`**: wrap any Tower service; caches GET/HEAD by default.
- 🔒 **Stampede protection**: deduplicates concurrent misses and serves stale data while recomputing.
- ⏱ **Flexible TTLs**: positive/negative TTL, refresh-before-expiry window, stale-while-revalidate.
- 🔄 **Auto-refresh**: proactively refreshes frequently-accessed cache entries before expiration.
- 🎬 **Chunk Caching**: memory-efficient caching for large files with range request support.
- 🏷️ **Cache Tags**: group and invalidate related cache entries together.
- 🎯 **Multi-Tier**: hybrid L1/L2 caching for optimal performance and capacity.
- 📊 **Admin API**: REST endpoints for cache introspection and management.
- 🤖 **ML-Ready Logging**: structured logs with request correlation for ML training.
- 📦 **Pluggable storage**: in-memory (Moka) and Redis backends.
- 📏 **Policy guards**: min/max body size, cache-control respect/override, custom method/status filters.
- 🧰 **Custom keys**: built-in extractors (path, path+query) plus custom closures.
- 📉 **Observability hooks**: optional metrics counters and tracing spans.

---

## Installation

```toml
[dependencies]
tower-http-cache = "0.7"

# Enable Redis support if required
tower-http-cache = { version = "0.7", features = ["redis-backend"] }

# With framework-agnostic admin API support
tower-http-cache = { version = "0.7", features = ["admin-api"] }
```

---

## Quick Start

```rust
use std::time::Duration;
use tower::ServiceBuilder;
use tower_http_cache::prelude::*;

let cache_layer = CacheLayer::builder(InMemoryBackend::new(10_000))
    .ttl(Duration::from_secs(120))
    .negative_ttl(Duration::from_secs(10))
    .stale_while_revalidate(Duration::from_secs(30))
    .refresh_before(Duration::from_secs(5))
    .min_body_size(Some(1024))
    .max_body_size(Some(256 * 1024))
    .respect_cache_control(true)
    .build();

let svc = ServiceBuilder::new()
    .layer(cache_layer)
    .service(tower::service_fn(|_req| async {
        Ok::<_, std::convert::Infallible>(http::Response::new("hello world"))
    }));
```

### Axum Integration (`HandleErrorLayer`)

Axum `Router::layer` requires middleware services to have `Error = Infallible`, whereas `CacheLayer` returns `BoxError`. Wrap `CacheLayer` with `axum::error_handling::HandleErrorLayer`:

```rust,no_run
use axum::{Router, error_handling::HandleErrorLayer, http::StatusCode, routing::get};
use std::time::Duration;
use tower::ServiceBuilder;
use tower_http_cache::layer::BoxError;
use tower_http_cache::prelude::*;

let cache_layer = CacheLayer::builder(InMemoryBackend::new(10_000))
    .ttl(Duration::from_secs(60))
    .build();

let app = Router::new()
    .route("/", get(|| async { "hello world" }))
    .layer(
        ServiceBuilder::new()
            .layer(HandleErrorLayer::new(|err: BoxError| async move {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Cache error: {err}"),
                )
            }))
            .layer(cache_layer),
    );
```

See [`examples/axum_redis.rs`](examples/axum_redis.rs) for a complete Redis + Axum server example.

### Chunk Caching for Large Files

Cache large responses once and serve `206 Partial Content` byte-range requests directly from in-memory chunks. Note that `StreamingPolicy::default()` skips `video/*`, `audio/*`, `application/octet-stream`, `application/pdf`, `application/zip`, `application/gzip`, `application/x-rar`, `application/x-tar`, and `application/x-7z-compressed` unless `excluded_content_types` is cleared:

```rust
use std::collections::HashSet;
use std::time::Duration;
use tower_http_cache::prelude::*;
use tower_http_cache::streaming::StreamingPolicy;

let cache_layer = CacheLayer::builder(InMemoryBackend::new(500))
    .policy(
        CachePolicy::default()
            .with_ttl(Duration::from_secs(3600))
            .with_streaming_policy(StreamingPolicy {
                enabled: true,
                enable_chunk_cache: true,
                chunk_size: 1024 * 1024,                   // 1MB chunks
                min_chunk_file_size: 5 * 1024 * 1024,      // Only chunk files >= 5MB
                max_cacheable_size: Some(64 * 1024 * 1024), // Allow files up to 64MB
                excluded_content_types: HashSet::new(),    // Allow video/audio/binary payloads
                ..Default::default()
            }),
    )
    .build();
```

**Benefits:**
- In-memory chunk serving for HTTP `206 Partial Content` range requests
- Bounded chunk index (`128 MiB` / `1,024` entries by default) with LRU and TTL eviction
- `If-Range` validation and seamless fallback to the backend entry when needed
- Range requests served directly from memory without re-reading the backend

**Example:**
See `examples/chunk_cache_demo.rs` for a complete working example.

### Using the Redis backend

```rust
use std::time::Duration;
use redis::aio::ConnectionManagerConfig;
use tower_http_cache::prelude::*;

async fn build_redis_layer(redis_url: &str) -> CacheLayer<RedisBackend> {
    let client = redis::Client::open(redis_url).expect("valid Redis URL");

    // redis 1.x defaults to a 500 ms response timeout and a 1 s connection
    // timeout. A response cache holds whole response bodies, so a large entry
    // over a loaded or cross-AZ Redis can exceed 500 ms, and every such
    // operation then fails instead of succeeding slowly. Choose the bound
    // against your own body sizes; `None` restores the 0.5.x behaviour of no
    // timeout at all.
    let config = ConnectionManagerConfig::new()
        .set_response_timeout(Some(Duration::from_secs(10)))
        .set_connection_timeout(Some(Duration::from_secs(5)));
    let manager = client
        .get_connection_manager_with_config(config)
        .await
        .expect("connect");

    CacheLayer::builder(RedisBackend::new(manager))
        .ttl(Duration::from_secs(30))
        .stale_while_revalidate(Duration::from_secs(10))
        .build()
}
```

### Enabling Auto-Refresh

Auto-refresh proactively refreshes frequently-accessed cache entries before they expire, reducing cache misses and latency for hot endpoints. Set a non-zero `.refresh_before(...)` window alongside `.auto_refresh(...)` (with the default `Duration::ZERO`, entries only qualify for refresh after expiry):

```rust
use std::time::Duration;
use tower_http_cache::prelude::*;
use tower_http_cache::refresh::AutoRefreshConfig;

let cache_layer = CacheLayer::builder(InMemoryBackend::new(10_000))
    .ttl(Duration::from_secs(120))
    .refresh_before(Duration::from_secs(30))
    .auto_refresh(AutoRefreshConfig {
        enabled: true,
        min_hits_per_minute: 10.0,
        check_interval: Duration::from_secs(10),
        max_concurrent_refreshes: 5,
        ..Default::default()
    })
    .build();

// Initialize auto-refresh with the service instance
cache_layer.init_auto_refresh(my_service.clone()).await?;
```

### Using Cache Tags

Group related cache entries and invalidate them together:

```rust
use tower_http_cache::prelude::*;
use tower_http_cache::tags::TagPolicy;

let cache_layer = CacheLayer::builder(backend)
    .policy(
        CachePolicy::default()
            .with_tag_policy(TagPolicy::new().with_enabled(true))
            .with_tag_extractor(|method, uri| {
                // Extract tags from request
                vec!["user:123".to_string(), "posts".to_string()]
            })
    )
    .build();

// Later: invalidate through the layer (also evicts ChunkCache when enabled;
// calling `backend.invalidate*` directly bypasses ChunkCache).
for tag in ["user:123", "posts"] {
    cache_layer.invalidate_by_tag(tag).await?;
}
```

Tag-based invalidation works on `InMemoryBackend`, and on `MultiTierBackend`
over one. `RedisBackend` keeps no reverse tag index, so `get_keys_by_tag`,
`list_tags` and `invalidate_by_tag` return `CacheError::Unsupported` rather
than a silent `Ok(0)`. Tags themselves do cross the Redis wire as of 0.6.0, so
a `CacheRead` from Redis carries the tags its entry was stored with. `TagIndex`
is also process-local, so invalidating a tag clears only the calling process's
index.

### Multi-Tier Caching

Combine fast in-memory cache with larger distributed storage:

```rust
use tower_http_cache::backend::multi_tier::{MultiTierBackend, PromotionStrategy};

let backend = MultiTierBackend::builder()
    .l1(InMemoryBackend::new(1_000))        // Hot data (fast)
    .l2(RedisBackend::new(manager))          // Cold storage (large)
    .promotion_strategy(PromotionStrategy::HitCount { threshold: 3 })
    .write_through(true)
    .build();

let cache_layer = CacheLayer::builder(backend)
    .ttl(Duration::from_secs(300))
    .build();
```

### Smart Streaming & Large File Handling

Automatically prevent large files from overwhelming your cache:

```rust
use tower_http_cache::streaming::StreamingPolicy;

let cache_layer = CacheLayer::builder(backend)
    .policy(
        CachePolicy::default()
            .with_streaming_policy(StreamingPolicy {
                enabled: true,
                max_cacheable_size: Some(1024 * 1024), // 1MB limit
                excluded_content_types: HashSet::from([
                    "application/pdf".to_string(),
                    "video/*".to_string(),
                    "audio/*".to_string(),
                    "application/zip".to_string(),
                ]),
                ..Default::default()
            })
    )
    .build();
```

**Features:**
- Automatic early detection via `Content-Length` and `size_hint()`
- Content-Type based filtering (skip PDFs, videos, archives by default)
- Protects multi-tier caches (large files excluded from L1)
- Prevents memory exhaustion from large response bodies
- Fully configurable per content-type and size

### Admin API

Enable framework-agnostic cache introspection and management handlers (no Axum dependency required):

```rust
use tower_http_cache::admin::{AdminConfig, AdminState};

let admin_config = AdminConfig::new()
    .with_require_auth(true)
    .with_auth_token("your-secret-token")
    .with_enabled(true);

// Wire AdminState to the CacheLayer's backend, live GlobalStats, and ChunkCache:
// (Note: calling `backend.invalidate()` directly bypasses ChunkCache eviction.)
let admin_state = AdminState::from_layer(&cache_layer, admin_config);

// Available handler functions (wire into any HTTP framework):
// tower_http_cache::admin::routes::handle_health
// tower_http_cache::admin::routes::handle_stats
// tower_http_cache::admin::routes::handle_hot_keys
// tower_http_cache::admin::routes::handle_list_tags
// tower_http_cache::admin::routes::handle_invalidate
```

### ML-Ready Structured Logging

Enable structured logging for cache operations and request correlation (automatically picks up `X-Request-ID` when present):

```rust
use tower_http_cache::logging::MLLoggingConfig;

let cache_layer = CacheLayer::builder(backend)
    .policy(
        CachePolicy::default()
            .with_ml_logging(MLLoggingConfig {
                enabled: true,
                sample_rate: 1.0, // Log 100% of operations
                hash_keys: true,  // Hash keys and URIs for privacy
                ..Default::default()
            })
    )
    .build();

// Logs are emitted as structured JSON:
// {
//   "timestamp": "2026-11-17T12:00:00.000Z",
//   "level": "info",
//   "target": "tower_http_cache::ml",
//   "event": "hit",
//   "request_id": "550e8400-...",
//   "key": "sha256-hex...",
//   "hit": true,
//   "latency_us": 150
// }
```

---

## Configuration Highlights

| Policy | Description |
| ------ | ----------- |
| `ttl` / `negative_ttl` | cache lifetime for successful and error responses |
| `stale_while_revalidate` | serve stale data while a refresh is in progress |
| `refresh_before` | proactively refresh the cache shortly before expiry |
| `auto_refresh` | automatically refresh frequently-accessed entries (requires non-zero `refresh_before`) |
| `cookie_policy` | configure how `Cookie` request headers interact with the cache (`Bypass`, `RequirePublic`, `Ignore`); `Vary` headers beyond `Accept-Encoding` are not part of the cache key |
| `tag_policy` | configure cache tags and invalidation groups |
| `multi_tier` | enable multi-tier caching with L1/L2 backends |
| `ml_logging` | enable ML-ready structured logging |
| `allow_streaming_bodies` | opt into caching streaming responses |
| `min_body_size` / `max_body_size` | enforce size bounds for cached bodies |
| `header_allowlist` | restrict which headers are stored alongside cached bodies |
| `method_predicate` / `statuses` | customize cacheable methods and status codes |

For the full API surface, see the generated docs: `cargo doc --open`.

---

## Benchmarks

Benchmarks are powered by Criterion and can be reproduced with:

```bash
cargo bench --bench cache_benchmarks
```

**Run benchmarks locally only. Never add `cargo bench` to CI** — Criterion
executes the whole suite, which takes minutes and produces timings that are
meaningless on shared runners. CI compiles the benches instead, via
`cargo test --no-run --benches`.

Latest results (macOS / M3 Pro / Rust 1.85, `redis-backend` disabled unless noted).
The `codec/*` rows were measured against 0.5.x's bincode codec; 0.6.0 replaced it
with postcard and renamed those benches to `codec/postcard_*`. They have not been
re-measured.

| Group | Benchmark | Median | Notes |
| ----- | --------- | ------ | ----- |
| `layer_throughput` | `baseline_inner` | 1.41 ms | Underlying service without caching |
| | `cache_hit` | 0.67 µs | Cached GET; body already materialized |
| | `cache_miss` | 0.68 µs | Miss with immediate store |
| `key_extractor` | `path` | 23.8 ns | GET/HEAD path only |
| | `path_and_query` | 97.4 ns | Path + query concatenation |
| | `custom_hit` | 84.7 ns | User extractor returning `Some` |
| | `custom_miss` | 1.35 ns | User extractor returning `None` |
| `backend/in_memory` | `get_small_hit` | 309 ns | 1 KiB entry |
| | `get_large_hit` | 327 ns | 128 KiB entry |
| | `set_small` | 676 ns | 1 KiB write |
| | `set_large` | 660 ns | 128 KiB write |
| `stampede` | `cache_layer` | 5.92 ms | 64 concurrent requests with caching |
| | `no_cache` | 5.76 ms | Same workload without layer |
| `stale_while_revalidate` | `stale_hit_latency` | 33.6 ms | Serve-stale branch |
| | `strict_refresh_latency` | 33.7 ms | Force refresh branch |
| `codec/bincode` (0.5.x) | `encode_small` | 362 ns | 1 KiB payload |
| | `decode_small` | 381 ns | 1 KiB payload |
| | `encode_large` | 146 µs | 128 KiB payload |
| | `decode_large` | 174 µs | 128 KiB payload |
| `negative_cache` | `initial_miss` | 14.0 µs | First miss populates negative entry |
| | `stored_negative_hit` | 21.9 ms | TTL-expired negative pathways |
| | `after_ttl_churn` | 5.66 µs | Subsequent positive hit |

Full raw output, including outlier analysis, is captured in [`initial_benchmark.md`](initial_benchmark.md).

---

## Testing & Tooling

```bash
# Library unit tests + integration tests
cargo test

# Redis integration tests
REDIS_URL=redis://127.0.0.1:6379/ cargo test --features redis-backend --tests redis_example

# Redis smoke test (launches example service, verifies cache hit/miss behaviour)
docker compose -f docker-compose.redis.yml up -d redis
python3 scripts/redis_smoke.py
docker compose -f docker-compose.redis.yml down

# Examples
cargo run --example axum_redis --features redis-backend
cargo run --example chunk_cache_demo
cargo run --example redis_smoke --features redis-backend
cargo run --example v0_3_features --features admin-api
```

---

## Feature Flags

| Feature | Description | Default |
| ------- | ----------- | :-----: |
| `in-memory` | Enables the Moka-powered in-memory backend | ✓ |
| `redis-backend` | Enables the Redis backend, codec, and async utilities | ✗ |
| `admin-api` | Enables framework-agnostic admin stats, key inspection, and invalidation handlers | ✗ |
| `serde` | Derives `serde` traits for cached entries/codecs and structured `CacheEvent` logging | ✓ |
| `compression` | Adds optional gzip compression for cached payloads | ✗ |
| `metrics` | Emits `metrics` counters (hit/miss/store/etc.) | ✗ |
| `tracing` | Adds tracing spans around cache operations | ✗ |

---

## Upgrading from 0.6.x to 0.7.0

### Removed 0.5.x legacy migration path

- Removed `legacy-bincode1-read`, the legacy reader module, and the deprecated `BincodeCodec` type alias. All Redis entries now use the versioned postcard envelope (`PostcardCodec`) introduced in `0.6.0`.
- `CacheBackend::invalidate_by_tags` uses slice elements cleanly and `InMemoryBackend` automatically prunes overwritten, expired, and evicted keys from its tag index.

### Framework-agnostic `admin-api` & live stats wiring

- `admin-api` no longer pulls `axum` as a dependency (`admin-api = ["serde"]`).
- Connect [`CacheLayer`](https://docs.rs/tower-http-cache) to [`AdminState::from_layer`](https://docs.rs/tower-http-cache) so HTTP hits, misses, stores, invalidations, and `ChunkCache` evictions stay wired together (calling `backend.invalidate()` directly bypasses `ChunkCache`).

### Other breaking changes in 0.7.0

- Added `#[non_exhaustive]` to public enums (`StreamingDecision`, `CacheEventType`, `CompressionStrategy`, `PromotionStrategy`, `RangeHandling`, `CookiePolicy`) and removed the hidden `StreamingDecision::StreamThrough` placeholder variant.
- Added new public fields: `CacheEntry::compressed` (custom `CacheCodec` implementations must persist and restore it), `StreamingPolicy::{max_chunk_cache_bytes, max_chunk_cache_entries}`, `AdminState::chunk_cache`, and `TierStats::writes`.
- `MultiTierBackend::stats(&self)` now returns a `TierStats` snapshot by value (with a new `writes` field) instead of `&TierStats`.
- `RangeRequest::normalize` resolves RFC 9110 suffix ranges (`bytes=-N`) alongside start-end and open-ended ranges; construct values with `RangeRequest::new` or `RangeRequest::suffix`.
- Enabled `compression` now writes `Content-Encoding: gzip` and updates `Content-Length` on compressed cached entries, while skipping responses that already carry `Content-Encoding` and validating client `Accept-Encoding` on hits.
- Redis envelope bumped to `FORMAT_V2` (`0x02`) while still reading `0.6.x` `FORMAT_V1` entries transparently (`compressed = false`). Requests carrying `Cookie` bypass the cache by default via `CookiePolicy::Bypass` (`RequirePublic` and `Ignore` are opt-in). `Vary` headers beyond `Accept-Encoding` are not part of the cache key: responses declaring `Vary: Cookie`, `Vary: Authorization`, or `Vary: *` are neither stored nor served when the corresponding request header is present.

See [CHANGELOG.md](CHANGELOG.md) for the full release history, including earlier `0.5.x` → `0.6.0` notes.

---

## Minimum Supported Rust Version

MSRV: **1.85** for the default feature set, matching the crate's `rust-version`
field. **`redis-backend` requires 1.88** — `redis` 1.8 declares `rust-version = "1.88"`.
Both floors are enforced by separate CI jobs.

The MSRV will only increase with a minor version bump and will be documented in
release notes.

---

## Status

`tower-http-cache` is under active development. Expect API adjustments while we stabilize the 0.x series. Contributions and feedback are welcome—feel free to open an issue or PR! ***

---

## License

This project is dual-licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT License ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

You may choose either license to suit your needs. Unless explicitly stated otherwise, any contribution intentionally submitted for inclusion in the crate shall be dual-licensed as above, without additional terms or conditions.

---

## Contributing

1. Fork and clone the repository.
2. Install prerequisites (`cargo`, `rustup`, and Docker if you plan to run Redis tests).
3. Run the checks:
   ```bash
   cargo fmt --all
   cargo clippy --all-targets --all-features
   cargo test
   python3 scripts/redis_smoke.py
   ```
4. Open a pull request with a succinct summary, test evidence, and (when applicable) benchmark output via `cargo bench`. Run benchmarks locally and paste the output; do not add a bench step to CI.

Bug reports and feature requests are welcome in the issue tracker. For larger design changes, please start a discussion thread to align on API shape before submitting code.
