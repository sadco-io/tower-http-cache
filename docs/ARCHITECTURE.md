# Architecture Overview

## Components
- `CacheLayer`: Tower layer wrapping inner services.
- `CacheService`: service implementation handling lookup, population, and response serialization.
- `CacheBackend` trait: abstract storage API with async `get`, `set`, `invalidate` methods.
- `CacheEntry`: encoded representation of cached responses (status, headers, body metadata, TTL).
- `KeyExtractor`: strategy object for deriving cache keys (path, query, custom closures).
- `CachePolicy`: ttl, negative ttl, stale-while-revalidate window, refresh-before threshold, body size limits, status/method/header filters, cache-control handling.
- Builder configuration (TTL, negative TTL, key extractor) exposed via `CacheLayer::builder`.
- `CacheCodec`: pluggable serialization strategy for persistence backends (default bincode).
- Serialization hooks allow custom codecs for JSON/bincode/other formats.

## Request Flow
1. Incoming request enters `CacheService::call`.
2. Extract key using configured `KeyExtractor`.
3. Check backend (`get`).
   - On hit: deserialize entry, rebuild response, return immediately.
   - On miss: acquire stampede guard and continue.
4. Await inner service response.
5. Serialize response body into `CacheEntry` (respecting policy for status codes, headers, etc.).
6. Write to backend (`set`) with TTL, release guard, return response.

## Stampede Protection
- Use per-key async mutex or coalescing future stored in backend guard map.
- On miss, first caller populates; others await result or serve stale depending on policy.
- Guard storage maintained via `dashmap` keyed by cache key to avoid global lock.

## Backends
### In-Memory
- Powered by Moka TinyLFU cache.
- TTL enforced per entry; size bound via capacity.
- Stores serialized bytes; optional compression.
- Stampede guard stored alongside (e.g., separate `DashMap`).

### Redis
- Uses async pooled connection manager.
- `SETEX`/`GET` operations with TTL.
- Negative cache entries stored with special prefix.
- Optional pipelining for batch invalidation.
- Requires serialization codec to produce compact payload.

## Serialization
- `CacheCodec` trait defines `encode(ResponseParts, BodyBytes) -> Bytes` and `decode(Bytes) -> Response`.
- Default JSON codec provided when `serde` feature enabled; binary codec for compact storage.
- Body capture uses `http_body_util::BodyExt` to collect bytes without blocking streaming use cases.
- Serialization safeguards: optional min/max body sizes, streaming allowance, and gzip compression via feature flag.

## Policies
- TTL per route or default.
- Status code filters (e.g., cache 200/203/404 only).
- Header passthrough: optional allowlist for headers to cache/restore.
- Negative cache TTL for errors.
- Stale-while-revalidate: configurable window allowing stale responses while a single refresher repopulates the cache, with optional refresh-before threshold to trigger proactive refresh.
- Cache policy knobs: respect Cache-Control headers, limit cacheable body sizes, allow custom method predicates and header allowlists.

## Observability
- Emit metrics: hits, misses, stale served, write errors.
- Optional tracing spans with key hash (not raw key) for privacy.
- Event hooks (Rust callbacks) for custom logging or invalidation triggers.
- Observability hooks: optional metrics (hits/misses/stale/store) and tracing spans around cache operations.

## Failure Handling
- Backend failure falls back to pass-through (no cache); optionally mark degraded mode.
- Provide circuit-breaker style fallback for persistent backend failures.

## Extensibility
- Public traits enable external backends.
- Feature flags control optional deps (serde, Redis, metrics).
- Potential integration with future load-shed layer via shared signals.
