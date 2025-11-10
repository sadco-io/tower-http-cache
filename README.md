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
- 🏷️ **Cache Tags**: group and invalidate related cache entries together.
- 🎯 **Multi-Tier**: hybrid L1/L2 caching for optimal performance and capacity.
- 📊 **Admin API**: REST endpoints for cache introspection and management.
- 🤖 **ML-Ready Logging**: structured logs with request correlation for ML training.
- 📦 **Pluggable storage**: in-memory backend (Moka) and optional Redis backend (async pooled).
- 📏 **Policy guards**: min/max body size, cache-control respect/override, custom method/status filters.
- 🧰 **Custom keys**: built-in extractors (path, path+query) plus custom closures.
- 📉 **Observability hooks**: optional metrics counters and tracing spans.

---

## Installation

```toml
[dependencies]
tower-http-cache = "0.3"

# Enable Redis support if required
tower-http-cache = { version = "0.3", features = ["redis-backend"] }

# With admin API support
tower-http-cache = { version = "0.3", features = ["admin-api"] }
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

### Using the Redis backend

```rust
use std::time::Duration;
use tower_http_cache::prelude::*;

async fn build_redis_layer(redis_url: &str) -> CacheLayer<RedisBackend> {
    let client = redis::Client::open(redis_url).expect("valid Redis URL");
    let manager = client.get_tokio_connection_manager().await.expect("connect");

    CacheLayer::builder(RedisBackend::new(manager))
        .ttl(Duration::from_secs(30))
        .stale_while_revalidate(Duration::from_secs(10))
        .build()
}
```

### Enabling Auto-Refresh

Auto-refresh proactively refreshes frequently-accessed cache entries before they expire, reducing cache misses and latency for hot endpoints:

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

// Later: invalidate all entries with a tag
backend.invalidate_by_tag("user:123").await?;
backend.invalidate_by_tags(&["user:123", "posts"]).await?;
```

### Multi-Tier Caching

Combine fast in-memory cache with larger distributed storage:

```rust
use tower_http_cache::backend::MultiTierBackend;

let backend = MultiTierBackend::builder()
    .l1(InMemoryBackend::new(1_000))        // Hot data (fast)
    .l2(RedisBackend::new(manager))          // Cold storage (large)
    .promotion_threshold(3)                   // Promote after 3 L2 hits
    .promotion_strategy(PromotionStrategy::HitCount)
    .write_through(true)
    .build();

let cache_layer = CacheLayer::builder(backend)
    .ttl(Duration::from_secs(300))
    .build();
```

### Admin API

Enable cache introspection and management endpoints:

```rust
use tower_http_cache::admin::{AdminConfig, admin_router};

let admin_config = AdminConfig::builder()
    .require_auth(true)
    .auth_token("your-secret-token")
    .build();

// Mount admin routes (Axum example)
let admin_routes = admin_router(backend.clone(), admin_config);
let app = Router::new()
    .nest("/admin/cache", admin_routes)
    .layer(cache_layer);

// Available endpoints:
// GET  /admin/cache/health
// GET  /admin/cache/stats
// GET  /admin/cache/hot-keys
// GET  /admin/cache/tags
// POST /admin/cache/invalidate
```

### ML-Ready Structured Logging

Enable structured logging for ML model training:

```rust
use tower_http_cache::logging::MLLoggingConfig;

let cache_layer = CacheLayer::builder(backend)
    .policy(
        CachePolicy::default()
            .with_ml_logging(MLLoggingConfig {
                enabled: true,
                sample_rate: 1.0,        // Log 100% of operations
                hash_keys: true,          // Hash keys for privacy
                include_request_id: true, // Correlate with X-Request-ID
            })
    )
    .build();

// Logs will be emitted in JSON format:
// {
//   "timestamp": "2025-11-10T12:00:00Z",
//   "request_id": "550e8400-...",
//   "operation": "cache_hit",
//   "latency_us": 150,
//   "tags": ["user:123"],
//   "tier": "l1"
// }
```

---

## Configuration Highlights

| Policy | Description |
| ------ | ----------- |
| `ttl` / `negative_ttl` | cache lifetime for successful and error responses |
| `stale_while_revalidate` | serve stale data while a refresh is in progress |
| `refresh_before` | proactively refresh the cache shortly before expiry |
| `auto_refresh` | automatically refresh frequently-accessed entries before expiration |
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

Latest results (macOS / M3 Pro / Rust 1.85, `redis-backend` disabled unless noted):

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
| `codec/bincode` | `encode_small` | 362 ns | 1 KiB payload |
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
cargo run --example axum_basic --features middleware
cargo run --example axum_custom --features middleware
cargo run --example redis_smoke --features redis-backend
```

---

## Feature Flags

| Feature | Description | Default |
| ------- | ----------- | :-----: |
| `in-memory` | Enables the Moka-powered in-memory backend | ✓ |
| `redis-backend` | Enables the Redis backend, codec, and async utilities | ✗ |
| `admin-api` | Enables admin REST API endpoints (requires axum) | ✗ |
| `serde` | Derives `serde` traits for cached entries/codecs | ✓ |
| `compression` | Adds optional gzip compression for cached payloads | ✗ |
| `metrics` | Emits `metrics` counters (hit/miss/store/etc.) | ✗ |
| `tracing` | Adds tracing spans around cache operations | ✗ |

---

## Minimum Supported Rust Version

MSRV: **1.75.0** (matching the crate's `rust-version` field).
The MSRV will only increase with a minor version bump and will be documented in release notes.

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
4. Open a pull request with a succinct summary, test evidence, and (when applicable) benchmark output via `cargo bench`.

Bug reports and feature requests are welcome in the issue tracker. For larger design changes, please start a discussion thread to align on API shape before submitting code.
