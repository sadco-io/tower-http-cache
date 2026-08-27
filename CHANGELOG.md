# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **The on-the-wire cache format changed, and `tags` are now part of it.**
  Entries stored in Redis and Memcached are now written as a 21-byte versioned
  envelope (`"THC"` magic, format byte, codec byte, and the expiry/stale
  timestamps as little-endian `u64`) followed by a `postcard`-encoded payload.
  Previously the two backends used two *different* undocumented `bincode 1`
  layouts.

  **Rolling back to 0.5.x is safe.** A 0.5.x binary encountering a 0.6.0 entry
  gets a clean decode error, which the cache layer already treats as a miss.
  You get a cold cache, not corrupted responses. (This is exactly what the
  envelope header buys: without it, an old reader would have silently accepted
  the new bytes and ignored the trailing remainder.)

  **Upgrading is safe and does not cold-start your cache.** 0.6.0 reads
  0.5.x-written entries transparently via the `legacy-bincode1-read` feature,
  which is **on by default**. Entries are rewritten in the new format as they
  are refreshed. The legacy reader is removed in 0.7.0; by then every 0.5.x
  entry will long since have aged past its TTL.

  The envelope is documented in `tower_http_cache::codec::envelope`, and byte 4
  records which codec wrote the entry. Entries written by one codec are
  reported as a miss rather than handed to another.

- **`BincodeCodec` is renamed `PostcardCodec`.** A deprecated type alias keeps
  `BincodeCodec` working through 0.6.x; it is removed in 0.7.0. Because
  `RedisBackend<C = PostcardCodec>` resolves through the alias, most downstream
  code is untouched.

- **`CacheCodec` gained a defaulted `CODEC_ID` associated constant.** It names
  the codec in the envelope header. The default is `0x80`, the start of the
  range reserved for codecs implemented outside this crate, so existing
  downstream `impl CacheCodec` blocks keep compiling unchanged.

- **`MemcachedBackend` now honours `CacheCodec`.** It previously called
  `bincode` directly, bypassing the codec entirely, so `with_codec` had no
  memcached equivalent and the two shared backends disagreed about the format.
  Both now route through one codec and one envelope. `MemcachedBackend` gained
  a defaulted codec type parameter (`MemcachedBackend<C = PostcardCodec>`) and
  a `with_codec` method mirroring `RedisBackend`; `MemcachedBackendBuilder` is
  unchanged and still builds the default-codec backend.

- Redis no longer double-encodes. 0.5.x encoded the entry into a `Vec<u8>`,
  then encoded that vector into a second `Vec<u8>`; the envelope removes one
  full copy of the body on every `set`.

- **BREAKING: `RedisBackend` and `MemcachedBackend` now report that they have
  no tag index.** `get_keys_by_tag` and `list_tags` return
  `Err(CacheError::Unsupported(..))`, and the trait's defaulted
  `invalidate_by_tag` propagates it. Previously all three inherited the trait
  defaults and answered `Ok(vec![])` / `Ok(0)`, so a caller could not tell
  "nothing carried that tag" from "this backend cannot do tags at all" — which
  is precisely the population being silently failed today. If you call
  `invalidate_by_tag` on a shared backend and ignore the count, you now get an
  error; handle it, or check `CacheError::is_unsupported`.

  `MultiTierBackend` deliberately tolerates this: a tier that reports
  `Unsupported` contributes nothing rather than failing the call, so the usual
  `InMemoryBackend` over `RedisBackend` arrangement keeps working off the L1
  index. Only if *neither* tier has an index does the error propagate.

- **BREAKING: `CacheError` is `#[non_exhaustive]` and gained an `Unsupported`
  variant.** Exhaustive matches on it need a `_` arm. Both changes are breaking
  and are made together, in the release that is breaking anyway, so future
  variants are additive. `CacheError::is_unsupported()` is provided so callers
  do not have to match at all.

### Added

- **`legacy-bincode1-read` feature, on by default.** Reads cache entries
  written by 0.5.x so an upgrade does not cold-start a production cache. The
  reader is hand-written against the bincode 1 layout and pulls no dependency,
  so leaving it enabled costs only dead code; it exists so 0.7.0 can delete one
  module and one feature entry. Turning it off is safe at any time and only
  costs a cold cache. Bytes no decoder recognises read as a miss, never an
  error and never a panic, and are never deleted.

  Backed by golden fixtures under `tests/fixtures/v0_5_1/`: real bytes produced
  by the published 0.5.1 code path, decoded field by field.

### Removed

- **`bincode` is no longer a dependency.** RUSTSEC-2025-0141 marked it
  permanently unmaintained in December 2025 with no patched release.

  To be precise about what this is and is not: RUSTSEC-2025-0141 is
  `informational = "unmaintained"`, **not a vulnerability**. There is no known
  exploit in `bincode 1.3.3`. The reason to move is that a permanently-ignored
  advisory trains people to ignore advisories.

  The reader for 0.5.x-format entries is hand-written against the (fixed,
  simple) bincode 1 layout rather than calling `bincode`, which is what allowed
  the dependency to be dropped in the same release that keeps backward
  compatibility. `cargo tree -i bincode` finds no match under any feature
  combination, including `--all-features`.

- **Note for anyone tracking dependabot: do not merge a `bincode 3.0` bump.**
  `bincode` 3.0.0 is a tombstone release. Its entire `src/lib.rs` is
  `compile_error!("https://xkcd.com/2347/");` — it was published only to signal
  the crate's status, since crates.io has no way to archive a crate. It has no
  features and no dependencies, and bumping to it does not compile. The last
  functional release is 2.0.1, which is covered by the same advisory (it has no
  version bound), so it was not a useful destination either.

### Fixed

- **0.5.x's memcached backend could not read back what it had written.**
  `CacheEntry`'s `version_serde` helper wrote its discriminant as `i32` —
  the match arms had no type annotation, so the literals defaulted to `i32` —
  while the matching `deserialize` read a `u8`. Under `bincode` that is four
  bytes written against one byte read, so `bincode::deserialize::<MemcachedRecord>`
  failed on every value the backend itself had stored, and the cache layer
  served the failure as a miss. `MemcachedBackend` was, in effect, a no-op
  cache for its entire existence.

  The helper is fixed (`let v: u8 = ...`), so `CacheEntry`'s derived
  `Serialize`/`Deserialize` now round-trip under non-self-describing formats,
  and both shared backends route through the codec and envelope rather than
  serializing `CacheEntry` directly. The legacy reader decodes the four-byte
  form, so entries 0.5.x wrote to memcached become readable for the first time.

- **`CachePolicy::with_tag_extractor` did nothing.** `CachePolicy::extract_tags`
  had no callers anywhere in the crate: both places where the layer builds a
  `CacheEntry` used `CacheEntry::new(..)` and never attached tags. Tags
  configured through the middleware — the mechanism the README documents —
  never reached any backend, including the in-memory one. The layer now calls
  `extract_tags` on both the store and the refresh path and attaches the
  result.

  This is inert unless you opted in: `TagPolicy::enabled` defaults to `false`,
  and `extract_tags` returns an empty vector when it is. There is a test for
  that inertness as well as for the fix.

- **Cache tags were silently dropped by the Redis codec.** `BincodeCodec::encode`
  serialized a private struct with no `tags` field, and `decode` rebuilt the entry
  through `CacheEntry::new`, which always sets `tags: None`. Tags never crossed the
  Redis wire. They now do. (Memcached was unaffected — it serialized `CacheEntry`
  whole. That inconsistency is also fixed.)

- The `cache_benchmarks` bench now declares `serde` in its `required-features`.
  It uses the codec, which lives behind that feature, so
  `cargo bench --no-default-features --features in-memory` did not build.

### Known issues

- **Tag-based invalidation works only on `InMemoryBackend` (and
  `MultiTierBackend` over one).** `RedisBackend` and `MemcachedBackend`
  implement `get`, `set` and `invalidate` only; they keep no reverse tag index,
  so `invalidate_by_tag` has nothing to iterate. 0.6.0 puts tags *on the wire*,
  which is a prerequisite for fixing this and means a `CacheRead` from a shared
  backend now carries the tags the entry was stored with — but it does not add
  a distributed tag index. `TagIndex` also remains process-local
  (`Arc<DashMap<..>>`), so even on the in-memory backend, invalidating a tag
  clears only the calling process's index.

  As of 0.6.0 the shared backends report this explicitly rather than returning
  a silent `Ok(0)`. A Redis-native tag index (Redis sets, opt-in, with
  TTL-based garbage collection of stale members) is planned for 0.7.0.
  Memcached has no set type and will continue to report tags as unsupported.

## [0.5.2] - 2026-08-26

A dependency-reduction and edition release. No wire-format change, no public
API change, no cache invalidation -- an existing cache stays readable across
the upgrade.

Two of these were deferred to 0.6.0 in the 0.5.1 notes. On inspection both
turned out to be non-breaking for this crate, so they ship here instead and
0.6.0 keeps its scope: the `bincode` migration and the backend bumps.

### Removed

- **`chrono` is gone entirely.** It was pulled in for one job -- rendering a
  `SystemTime` as an RFC 3339 / ISO 8601 string -- across four call sites, with
  no parsing, no local time and no timezone handling anywhere in the crate.
  That is now a private `time_fmt` module (civil-from-days, ~90 lines) and one
  fewer dependency, along with its `iana-time-zone` and `num-traits` subtree.

  These strings go into ML training logs (`CacheEvent::log`) and admin API JSON
  responses (`/health`, hot keys, stats), so the bar was **byte-identical
  output**, not merely correct output. A change in shape would be a silent
  break for anything parsing them. The replacement was validated by
  differential-testing it against `chrono` 0.4.45 over **2,364,037 cases**
  spanning chrono's entire representable range (years -262143 to +262142),
  every fractional-second precision, leap days, year boundaries, pre-epoch
  instants and the range boundaries: zero mismatches.

  Three chrono behaviours turned out to be load-bearing, and are preserved
  deliberately rather than tidied up, each pinned by a regression test:

  - `to_rfc3339` spells the UTC offset `+00:00`, **not** `Z`. (The ML log
    timestamp is a different format string and does end in `Z`.)
  - Its fractional-second precision is *variable* -- the shortest lossless
    choice of zero, 3, 6 or 9 digits. A fixed-width fraction would have been
    wrong for most inputs.
  - Extended years pad the magnitude to four digits after the sign (`-0001`,
    `+10000`), not five.

  The admin stats serializer's `secs as i64` was an unchecked cast, so a `u64`
  above `i64::MAX` wrapped to a negative timestamp instead of saturating. That
  is reproduced exactly rather than "fixed", because fixing it would change
  output.

- **`futures-util` is no longer a runtime dependency.** The library imported it
  for exactly one thing: `BoxFuture` as `CacheService::Future`. That is now a
  local `Pin<Box<dyn Future<Output = T> + Send + 'static>>` alias -- the same
  concrete type, so the associated type is unchanged for callers and this is
  not an API break. It moves to `[dev-dependencies]` rather than being deleted,
  because `tests/integration_cache.rs` still uses `futures_util::stream::unfold`
  to build a chunked body. It remains in the tree transitively via `moka` and
  `tower`; what changes is that this crate no longer declares it.

### Changed

- `edition` `2021` -> `2024`. MSRV is already `1.85`, exactly the edition-2024
  floor, so this costs no compatibility. `cargo fix --edition` was run across
  the feature matrix and every hunk reviewed by hand; the only substantive
  changes were three `if let (Some(ref x), ...)` patterns over tuples of
  references dropping their now-rejected `ref`, plus rustfmt's 2024 import
  ordering. One suggested rewrite was **rejected**: `cargo fix` converted
  `InMemoryBackend::get`'s `if let`/`else` into a `match` on account of the
  changed `if let` temporary scope, but moka's `get` returns an owned
  `Option<StoredEntry>` -- no guard, no lock -- and the binding moves the value
  out, so the rescope has nothing to observe. Similarly,
  `clippy::let_and_return` began firing on `StampedeGuard::acquire_handle`
  because clippy suppresses that lint pre-2024 when a `let` affects drop order;
  the binding pins the drop of a `DashMap` shard write guard and is kept, with
  a scoped allow, rather than letting lock release depend on edition-specific
  tail-expression rules in a path that then awaits.
- `sha2` `0.10` -> `0.11` (closes #9). The `Digest` trait path,
  `new`/`update`/`finalize` and the `hex::encode` of the output all survived the
  `digest` 0.11 bump unchanged, so `logging::hash_key` needed no edits. **Digest
  values are identical, so no cache invalidation.** sha2 0.11 declares
  `rust-version = 1.85`, exactly our floor.
- Dev-dependency `criterion` `0.7` -> `0.8` (closes #15). No source changes
  were needed: the bench already used `std::hint::black_box` rather than the
  deprecated `criterion::black_box`. criterion 0.8 declares `rust-version 1.86`,
  above our 1.85 floor, which is fine because dev-dependencies are not built by
  the MSRV job's `cargo build` -- the same allowance the 1.85 job already
  documents.

### Known issues

Unchanged from 0.5.1, and all four `deny.toml` suppressions still apply:

- **`bincode 1.3.3` is unmaintained (RUSTSEC-2025-0141).** Still deferred to
  0.6.0 -- it defines the on-disk and on-wire encoding for the Redis and
  Memcached backends, so migrating invalidates every live cache entry, which is
  not a patch-release change. It is reachable from the default feature set; a
  `--no-default-features --features in-memory` build still does not pull it at
  all, which remains the recommended configuration for anyone who does not need
  a shared backend.
- **`memcached-backend` is still NOT recommended for production**, for the same
  three advisories reached through `async-memcached` -> `toxiproxy_rust` ->
  `reqwest 0.11` (RUSTSEC-2026-0258 h2 DoS, RUSTSEC-2025-0134, RUSTSEC-2025-0057),
  and it still drags in a second HTTP stack requiring `libssl-dev` and
  `pkg-config`.
- `tests/integration_cache.rs::concurrent_requests_share_refresh_work` remains
  timing-sensitive under heavy machine load. Measured over ~9,000 runs at
  6-way parallelism: **2.2% failure rate on this release against 2.8% on
  0.5.1**, so it is pre-existing and marginally improved, not a regression. The
  0.5.1 fix removed the ~50% flakiness; what is left is the deterministic
  coalescing assertion timing out when the 300 ms stale window is missed on a
  saturated host.

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
- **Every integration test, example and bench now declares the backend feature it needs.**
  Once `in-memory` and `serde` became real gates, `cargo test` on a reduced feature set
  failed to resolve `InMemoryBackend` / `CacheEvent` in four integration tests, five
  examples, the benches, and two internal `#[cfg(test)]` modules. All now declare
  `required-features` or are `#[cfg]`-gated. Found by the new CI job -- the earlier local
  sweep used `cargo check`, which does not compile test modules.
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

[Unreleased]: https://github.com/sadco-io/tower-http-cache/compare/v0.5.2...HEAD
[0.5.2]: https://github.com/sadco-io/tower-http-cache/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/sadco-io/tower-http-cache/compare/v0.5.0...v0.5.1
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

- **Cache tags were silently dropped by the Redis codec.** `BincodeCodec::encode`
  serialized a private struct with no `tags` field, and `decode` rebuilt the entry
  through `CacheEntry::new`, which always sets `tags: None`. Tags never crossed the
  Redis wire. They now do. (Memcached was unaffected — it serialized `CacheEntry`
  whole. That inconsistency is also fixed.)

### Known issues

- **Tag-based invalidation works only on `InMemoryBackend` (and
  `MultiTierBackend` over one).** `RedisBackend` and `MemcachedBackend`
  implement `get`, `set` and `invalidate` only; they keep no reverse tag index,
  so `invalidate_by_tag` has nothing to iterate. 0.6.0 puts tags *on the wire*,
  which is a prerequisite for fixing this and means a `CacheRead` from a shared
  backend now carries the tags the entry was stored with — but it does not add
  a distributed tag index. `TagIndex` also remains process-local
  (`Arc<DashMap<..>>`), so even on the in-memory backend, invalidating a tag
  clears only the calling process's index.

  As of 0.6.0 the shared backends report this explicitly rather than returning
  a silent `Ok(0)`. A Redis-native tag index (Redis sets, opt-in, with
  TTL-based garbage collection of stale members) is planned for 0.7.0.
  Memcached has no set type and will continue to report tags as unsupported.

