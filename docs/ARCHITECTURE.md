# Architecture Overview

## Components
- `CacheLayer`: Tower layer wrapping inner services.
- `CacheService`: service implementation handling lookup, population, and response serialization.
- `CacheBackend` trait: abstract storage API with async `get`, `set`, `invalidate` methods.
- `CacheEntry`: encoded representation of cached responses (status, headers, body metadata, TTL).
- `KeyExtractor`: strategy object for deriving cache keys (path, query, custom closures).
- `CachePolicy`: ttl, negative ttl, stale-while-revalidate window, refresh-before threshold, body size limits, status/method/header filters, cache-control handling.
- Builder configuration (TTL, negative TTL, key extractor) exposed via `CacheLayer::builder`.
- `CacheCodec`: pluggable serialization strategy for the shared backends (default `PostcardCodec`).
- `codec::envelope`: the versioned header wrapped around every codec payload written to a shared backend. Normative reference for the wire format.

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
- The lookup in step 3 and the lock acquisition are not atomic, so both paths
  re-read the key once they hold or have waited on the lock: the holder may
  have been populated by a caller that finished inside that window.

## Backends
### In-Memory
- Powered by Moka TinyLFU cache.
- TTL enforced per entry; size bound via capacity.
- Stores serialized bytes; optional compression.
- Stampede guard stored alongside (e.g., separate `DashMap`).

### Redis
- Uses `redis::aio::ConnectionManager`, which is `Clone` and internally multiplexed; the backend holds it directly rather than behind a mutex.
- `SETEX`/`GET` operations with TTL.
- Negative cache entries stored with special prefix.
- Values are envelope-framed codec payloads (see Serialization).
- Keeps no reverse tag index: `get_keys_by_tag` and `list_tags` return `CacheError::Unsupported`.

## Serialization

Two layers, deliberately separate. A `CacheCodec` turns a `CacheEntry` into bytes
and back and knows nothing about expiry; the envelope wraps that payload in a
versioned header carrying the timing metadata. Custom codecs implement only
`CacheCodec` — the envelope is applied by the backend.

### Wire format (normative)

Every value written to a shared backend by 0.6.0 and later is:

```
 offset  size  field             encoding                       notes
 ------  ----  ----------------  -----------------------------  --------------------------------
      0     3  MAGIC             0x54 0x48 0x43  ("THC")        constant
      3     1  FORMAT_VERSION    0x01                           bump only on envelope changes
      4     1  CODEC_ID          0x01 = postcard                0x02..=0x7F reserved for this crate
                                                                0x80..=0xFF free for user codecs
      5     8  expires_at_ms     u64 little-endian              ms since UNIX_EPOCH
     13     8  stale_until_ms    u64 little-endian              ms since UNIX_EPOCH
     21     N  payload           CacheCodec::encode(&entry)     N = buf.len() - 21
 ------  ----
     21        ENVELOPE_HEADER_LEN
```

There is no payload length field: the transport frames the value exactly, and
postcard detects truncation rather than parsing a partial buffer. `CODEC_ID` is
checked on read, so an entry written by one codec is reported as a miss rather
than handed to another. Putting the timestamps in the header rather than the
payload keeps `CacheCodec`'s signature free of expiry, lets timing be read
without invoking the codec, and removes the double encode 0.5.x performed on the
Redis path.

- Payload: `postcard`, carrying status, version, headers, body and tags.
- 0.5.x wrote a bare `bincode 1` record with no header and no tags. The
  `legacy-bincode1-read` feature supplies a hand-written reader for those bytes;
  it pulls no dependency and is removed in 0.7.0. Unrecognised bytes are a miss,
  logged and counted, never an error and never a delete.
- The header is also what makes rollback safe: a 0.5.x reader fails cleanly on
  0.6.0 bytes instead of silently accepting them and ignoring the remainder.

### Body handling
- Body capture uses `http_body_util::BodyExt` to collect bytes without blocking streaming use cases.
- Serialization safeguards: optional min/max body sizes, streaming allowance, and gzip compression via feature flag. Compression runs before encoding, so the codec sees already-compressed bytes.

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
- Feature flags control optional deps (serde, Redis, metrics) and the 0.5.x legacy reader.
- Potential integration with future load-shed layer via shared signals.
