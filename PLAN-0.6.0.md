# tower-http-cache 0.6.0 — implementation plan

**Status:** planning document. Nothing in `src/`, `tests/`, `benches/`, `examples/`,
`Cargo.toml` or `CHANGELOG.md` has been modified to produce it.

**Baseline:** `master` @ `fcb70ab`, plus the 0.5.2 release branch (sha2 0.11,
criterion 0.8, `futures-util` and `chrono` dropped from `[dependencies]`,
edition 2021 -> 2024). 0.5.2 is assumed landed. Line numbers below are against
`master` and will have drifted by a few lines; every reference names the item it
points at so it can be re-found.

**Evidence convention.** Every empirical claim carries one of:

| Mark | Meaning |
| --- | --- |
| `[compiled]` | I built it and it compiled / ran |
| `[measured]` | I ran it and recorded numbers |
| `[metadata]` | read from crates.io API or an unpacked `.crate` |
| `[source]` | read from the crate's or this repo's source |
| `[docs]` | from upstream prose docs / release notes only |
| `[UNVERIFIED]` | asserted but not checked — see §12 Open questions |

### 0.1 Two environment notes for whoever executes this

1. **A pre-existing clippy failure is sitting in the tree right now.**
   `cargo clippy --all-features --all-targets -- -D warnings` fails with
   *"returning the result of a `let` binding from a block"* at `src/layer.rs:1070`.
   Reproduced byte-identically on an unmodified copy of HEAD, so it is **not**
   caused by anything in this plan — it comes from the in-flight edition-2024
   migration in 0.5.2 `[compiled]`. It must be fixed before the `-D warnings` CI job
   can pass, but it belongs to 0.5.2, not here. Do not let it be mistaken for W9
   fallout.

2. **`pkg-config` / OpenSSL headers are needed for anything touching
   `memcached-backend`**, because `async-memcached` -> `toxiproxy_rust` ->
   `reqwest 0.11` -> `openssl-sys` (§10). CI already installs `libssl-dev` and
   `pkg-config`; locally you may need
   `OPENSSL_LIB_DIR=... OPENSSL_INCLUDE_DIR=... OPENSSL_NO_VENDOR=1` `[compiled]`.

---

## 1. Recommendation summary

| Question | Answer |
| --- | --- |
| bincode 3 or postcard? | **Neither as posed — `bincode` 3.0.0 is a tombstone. Use `postcard` 1.1.3.** |
| Envelope | 21-byte header: `"THC"` magic, format byte, codec byte, two LE `u64` timestamps |
| A3 (distributed tag index) | **Made the cut as a documented limitation + a required honesty fix; the Redis-native index is deferred to 0.7.0** |
| C (RPITIT / drop `async-trait`) | **Made the cut — verified compiling, zero workarounds, all tests pass, MSRV 1.85 holds.** Needs W7 (bb8 0.9) to drop the dependency *entirely* |
| Legacy reader | **Hand-rolled, no `bincode` dependency** — this is what lets the RUSTSEC ignore be deleted in 0.6.0 |

### 1.1 `bincode` 3.0.0 is not a real release — dependabot #10 is a trap

I downloaded and unpacked `https://static.crates.io/crates/bincode/bincode-3.0.0.crate`.
It contains seven files. The entire `src/lib.rs` is 41 bytes `[compiled]`:

```rust
compile_error!("https://xkcd.com/2347/");
```

Its `README.md` says, verbatim `[metadata]`:

> Bincode is now unmaintained. Due to a doxxing and harassment incident,
> development on bincode has ceased. No further releases will be published on
> crates.io. As crates.io [...] lacks the ability to mark a project as archive or
> remove the last maintainer, this final release is being published containing
> only this README, as well as a lib.rs containing only a compiler error, to
> inform potential users of the maintenance status of this crate.

It declares zero dependencies and zero features — `bincode = { version = "3", features = ["serde"] }`
fails to resolve with *"bincode does not have that feature"* `[compiled]`.

**Merging dependabot #10 would replace the crate's serialization layer with a
compile error.** Close it with that explanation. This is the single most important
finding in this document and it invalidates the framing of the original brief.

### 1.2 bincode 2.0.1 does not clear the advisory either

`bincode` 2.0.1 (2025-03-10, `rust-version = 1.85.0`) is the last functional release `[metadata]`.
But RUSTSEC-2025-0141 is filed against the **package**, with no version bound `[metadata]`:

```toml
[advisory]
id = "RUSTSEC-2025-0141"
package = "bincode"
date = "2025-12-16"
informational = "unmaintained"

[versions]
patched = []
```

`patched = []` and no `unaffected` range means **every** version of `bincode` is
flagged, 2.0.1 included. Moving 1.3.3 -> 2.0.1 would be churn that changes the wire
format, breaks every live cache entry, and still leaves the `deny.toml` ignore in
place. Rejected.

Note also that this is `informational = "unmaintained"`, **not a vulnerability**.
There is no known exploit in bincode 1.3.3. The reason to move is that the ignore
entry is permanent otherwise, and a permanently-ignored advisory trains people to
ignore advisories. Frame it that way in the CHANGELOG — do not imply a security fix.

### 1.3 Why postcard

The advisory's own alternatives list is wincode, postcard, bitcode, rkyv `[metadata]`.

| | postcard 1.1.3 | wincode 0.6.1 | bitcode 0.6.7 | rkyv 0.8.18 |
| --- | --- | --- | --- | --- |
| MSRV | none declared; **builds on 1.85.0** `[compiled]` | **1.89.0** `[metadata]` | 1.70 `[metadata]` | 1.81 `[metadata]` |
| Stability | 1.x since 2023 | 0.x, 0.5.0 -> 0.6.1 in 5 months `[metadata]` | 0.x | 0.8.x |
| serde-based | yes | — | optional | no (own derive) |
| New crates in our tree | **1** (`cobs`) `[compiled]` | more | `bytemuck` + derive | large |

wincode's 1.89.0 MSRV exceeds this crate's declared 1.85 floor, and it is a 0.x
crate from the Solana org moving at ~one minor per two months. rkyv is a
zero-copy archival format — a different programming model than `CacheCodec`
expresses, and overkill for a struct this small. bitcode is a compression-oriented
format with its own derive.

**postcard wins on the axes that matter here:** stable 1.x, serde-native (so
`CacheEntry`'s existing `#[serde(with = ...)]` helpers keep working unchanged),
and a dependency footprint of exactly one new crate.

`cargo tree` on `postcard = { version = "1.1.3", default-features = false, features = ["use-std"] }` `[compiled]`:

```
postcard v1.1.3
├── cobs v0.3.0
│   └── thiserror v2.0.20        <- already a direct dependency of this crate
└── serde v1.0.229               <- already a direct dependency of this crate
```

Net new to the graph: `postcard` and `cobs`. `bincode` 1.3.3 leaves, taking
`byteorder` with it. Roughly a wash.

> **`default-features = false` is mandatory.** postcard's default feature set is
> `["heapless-cas"]`, which pulls `heapless 0.7` — embedded baggage we do not want `[metadata]`.
> With `default-features = false, features = ["use-std"]` no `heapless` appears in the
> tree `[compiled]`. `use-std = ["serde/std", "alloc"]`, which is what we need.

**Measured encoded size** — realistic entry, 8 typical response headers, `tags:
Some(["user:123", "tenant:acme"])`, timestamps `[measured]`:

| body | postcard (7 fields incl. tags + timings) | bincode 1.3.3 (4 fields, no tags/timings) | delta |
| --- | --- | --- | --- |
| 0 | 331 | 422 | **-91** |
| 512 | 846 | 936 | **-90** |
| 4096 | 4431 | 4521 | **-90** |
| 262144 | 262482 | 262571 | **-89** |

postcard is ~90 bytes *smaller* per entry **while carrying three more fields**,
because it varint-encodes lengths where bincode 1 spends a fixed `u64` on each.
The saving is per header and per collection, so it grows with header count and is
independent of body size.

**Measured throughput** — realistic entry, `--release`, 100k iterations, warmup
pass, **median of 9 runs**, plain `std::time::Instant` loop (deliberately not
criterion), aarch64 `[measured]`:

| codec | encode ns/op | decode ns/op |
| --- | --- | --- |
| bincode 1.3.3 (incumbent) | 1298 | 3072 |
| **postcard 1.1.3** | 1425 (≈10% slower) | **2241 (≈27% faster)** |
| bincode 2.0.1 via its serde bridge | 1426 | **7088 (2.3x SLOWER)** |
| wincode 0.6.1 | 113 | 331 |

Two things worth pausing on:

* **bincode 2.0.1's serde bridge is the worst option in the set on decode** — 2.3x
  slower than the incumbent it would replace. It is not the varint config
  (`config::legacy()` decodes at 8070 ns); the bridge itself is the cost. The
  "safe incremental step" is a performance trap. Another reason §1.2's conclusion
  stands.
* **A cache is decode-heavy.** Every hit decodes; only misses encode. postcard
  trading 127 ns of encode for 831 ns of decode is the right side of that trade.

### 1.3.1 A recorded dissent

The agent that ran the codec benchmark recommended **staying on bincode 1.3.3 for
0.6.0** and adding `tags` behind a version byte, deferring the postcard migration
to a later release. Its reasoning: old-bytes-decoded-as-new-struct already fails
cleanly on every candidate, so the `tags` fix does not *require* a codec change,
and coupling a format migration to a field addition doubles the risk.

That is a fair argument and it is recorded here rather than buried. **I disagree,
for two reasons:**

1. The stated goal of 0.6.0 is to clear the permanently-ignored advisory. Deferring
   the codec keeps `bincode` in `Cargo.toml` and keeps the `deny.toml` ignore, so
   the release would not achieve its purpose.
2. The risk of coupling is much lower than it looks, because the envelope makes the
   two changes *one* change. Once you are writing a versioned header — which you
   need anyway, per the agent's own finding that new-bytes-read-by-old-reader
   silently succeeds — the incremental risk of also swapping the payload encoding
   inside that envelope is small and fully covered by the golden-vector tests in
   §7.1.

If the release is time-boxed and something must be cut, cut **W8** (tags) rather
than **W2** (format): W8 is a behaviour fix that can ship in 0.6.1, whereas the
format change is the one that is expensive to do twice.

### 1.4 The one behaviour to check when swapping codecs

`CacheEntry.body` uses an **asymmetric** serde helper (`src/backend/mod.rs`,
`mod bytes_serde`): it serializes with `serializer.serialize_bytes(..)` but
deserializes with `Vec::<u8>::deserialize(..)`. That happens to round-trip under
bincode 1; it is not guaranteed to under an arbitrary format.

**Verified: it round-trips correctly under postcard** `[compiled]` — full
`StoredEntry` equality including `tags` and a 4096-byte body. No change needed.
Keep the regression test in §8 anyway, because this is exactly the kind of thing
that silently breaks on the *next* codec change.

---

## 2. The bug picture is worse than the brief states

The brief describes A1 as "tag invalidation is a no-op on Redis, caused by the
codec". That is true but it is one of **three independent** defects, and fixing
the codec fixes only the least important of them. Getting this wrong would ship a
CHANGELOG entry claiming tags are fixed when they are not.

### A1 — the codec drops `tags` (confirmed)

`src/codec.rs:18-24` defines a private `StoredEntry { status, version, headers, body }`
with no `tags` field; `decode` at `src/codec.rs:41` rebuilds through
`CacheEntry::new(..)`, which hardcodes `tags: None` at `src/backend/mod.rs:139` `[source]`.
`CacheEntry.tags` never crosses the Redis wire. Confirmed exactly as described.

### A2 — memcached bypasses `CacheCodec` (confirmed, and the two paths disagree)

`RedisBackend<C = BincodeCodec>` is generic with a `with_codec` escape hatch
(`src/backend/redis.rs:15,37`), but `src/backend/memcached.rs:431,470` calls
`bincode::deserialize` / `bincode::serialize` directly on a private
`MemcachedRecord { entry: CacheEntry, expires_at_ms, stale_until_ms }` `[source]`.

Because `MemcachedRecord` derives on `CacheEntry` **whole**, the memcached path
*does* carry `tags`. So the two shared backends genuinely disagree about what the
wire format contains — confirmed, and confirmed as the brief describes.

The shapes also differ structurally, so unification is not mechanical:

```
Redis     value = bincode1(RedisRecord     { payload: bincode1(StoredEntry), expires_at_ms, stale_until_ms })
Memcached value = bincode1(MemcachedRecord { entry: CacheEntry,              expires_at_ms, stale_until_ms })
```

Redis double-encodes: the body is copied into a `Vec<u8>`, that vector is encoded
into a second `Vec<u8>`, and the outer encode copies the whole payload again.
The unified format removes one full copy of the body on every `set`.

### A1b — **NEW: the layer never attaches tags to any entry, on any backend**

Not in the brief. `CachePolicy::extract_tags` (`src/policy.rs:187`) has **zero
callers anywhere in the crate** `[source]`:

```
$ grep -rn "extract_tags" . --include=*.rs
src/policy.rs:187:    pub fn extract_tags(&self, method: &Method, uri: &http::Uri) -> Vec<String> {
```

Both sites where the layer builds an entry — `src/layer.rs:573` (refresh path) and
`src/layer.rs:988` (store path) — call `CacheEntry::new(..)` and never
`.with_tags(..)` `[source]`. `with_tags` is called exactly once in the whole repo, in
`examples/v0_3_features.rs:207` `[source]`.

So `CachePolicy::with_tag_extractor(..)` — which the README documents at
README.md:154 as *the* way to use tags — silently does nothing. Tags only reach a
backend when the user bypasses the layer and calls `backend.set()` directly with a
hand-built `.with_tags()` entry. **Tag support is broken end-to-end through the
middleware on every backend, including in-memory.**

### A3 — **the shared backends have no tag index at all** (this is the real blocker)

`RedisBackend` and `MemcachedBackend` implement only `get`, `set`, `invalidate` `[source]`:

| backend | `get_keys_by_tag` | `list_tags` | `invalidate_by_tag` |
| --- | --- | --- | --- |
| `InMemoryBackend` | overridden (`TagIndex`) | overridden | inherited default |
| `MultiTierBackend` | overridden (L1+L2 union) | overridden | overridden |
| `RedisBackend` | **inherited -> `Ok(vec![])`** | **inherited -> `Ok(vec![])`** | **inherited -> always `Ok(0)`** |
| `MemcachedBackend` | **inherited -> `Ok(vec![])`** | **inherited -> `Ok(vec![])`** | **inherited -> always `Ok(0)`** |

The trait's default `get_keys_by_tag` returns an empty vector
(`src/backend/mod.rs:208-210`), and the default `invalidate_by_tag` iterates that
empty vector and returns `Ok(0)` (`src/backend/mod.rs:215-222`) `[source]`.

**Therefore: putting `tags` on the wire (A1) does not make `invalidate_by_tag`
work on Redis.** It makes tags survive a round trip so `CacheRead.entry.tags` is
populated, which is a prerequisite for a future index and is worth doing — but
`backend.invalidate_by_tag("users")` on a `RedisBackend` will still return `Ok(0)`
after the A1 fix. The `TagIndex` being process-local (`src/tags.rs:17-22`) is a
distant third problem behind "there is no index on this backend whatsoever".

**Do not let the CHANGELOG claim tag invalidation is fixed on shared backends in
0.6.0.** See W7 for the recommendation and W8 for the honesty fix.

---

## 3. Ordered work items

Each item is independently committable and independently revertable. Order is
load-bearing where noted.

| # | Item | Breaking? | Depends on |
| --- | --- | --- | --- |
| W1 | Close dependabot #10; record why | no | — |
| W2 | Envelope + postcard codec + unified backend record | **wire format** | — |
| W3 | Hand-rolled legacy reader, feature-gated | no | W2 |
| W4 | Drop `bincode` from `Cargo.toml` | no | W2, W3 |
| W5 | `redis` 0.32.7 -> 1.6.0 | no source change | — |
| W6 | Remove the redundant `Arc<Mutex<ConnectionManager>>` | no | W5 |
| W7 | `bb8` 0.8 -> 0.9 (un-deferred) | **downstream impls** | — |
| W8 | Wire the layer's tag extractor; document the shared-backend gap | no | W2 |
| W9 | `CacheBackend` -> RPITIT, drop `async-trait` | **downstream impls** | W7 |
| W10 | Docs, deny.toml, release checklist | no | all |

W2 before W3 (the legacy reader is defined relative to the new one). W5 before W6.
W7 before W9 (bb8 0.9 is what allows `async-trait` to leave entirely — see §6).

---
## 4. Wire format specification

### 4.1 Design decision: the envelope carries timing, the codec carries the entry

The two backends disagree about where timing metadata lives (§A2). Unify by
putting it in the **envelope header**, not in the codec payload:

* `CacheCodec`'s signature is unchanged — still `encode(&CacheEntry) -> Vec<u8>` /
  `decode(&[u8]) -> CacheEntry`. **Downstream custom codecs keep compiling.**
* Timings are backend metadata, not codec business. A user's encrypting or
  compressing codec should not have to know about `expires_at_ms`.
* Timings are readable without invoking the codec — useful for `redis-cli`
  forensics and for a future TTL-repair tool.
* Redis stops double-encoding; one full copy of the body disappears from `set`.

### 4.2 Byte layout

Every value written to Redis or Memcached by 0.6.0 is:

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

Constants belong in a new `src/codec/envelope.rs` (or the existing `src/codec.rs`):

```rust
pub(crate) const MAGIC: [u8; 3] = *b"THC";
pub(crate) const FORMAT_V1: u8 = 0x01;
pub(crate) const CODEC_POSTCARD: u8 = 0x01;
pub(crate) const ENVELOPE_HEADER_LEN: usize = 21;
```

No payload length field. The transport already frames the value exactly (Redis
`GET` and memcached `get` both return the stored length), and postcard detects
truncation cleanly on its own — a buffer cut to 1, 10, 100 or 50% of its length
all produced `Err("Hit the end of buffer, expected more data")`, never a
successful partial parse `[measured]`.

### 4.3 The codec payload

`BincodeCodec`'s private `StoredEntry` is replaced by a `tags`-carrying struct.
This is where A1 is fixed:

```rust
#[derive(Serialize, Deserialize)]
struct StoredEntry {
    status: u16,
    version: u8,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
    tags: Option<Vec<String>>,   // <-- NEW: fixes A1
}
```

and `decode` must stop discarding it — replace the `CacheEntry::new(..)` call at
`src/codec.rs:41` with a struct literal, or `CacheEntry::new(..).with_tags(..)`
when `tags` is `Some`. **`CacheEntry::new` hardcoding `tags: None` is correct and
should stay** — it is the constructor's documented behaviour; the bug is the codec
routing through it.

### 4.4 Read-path dispatch

One function, parameterised by which legacy shape the calling backend used:

```rust
enum LegacyShape { RedisOuter, MemcachedOuter }

fn read_stored(bytes: &[u8], codec: &C, shape: LegacyShape)
    -> Result<Option<CacheRead>, CacheError>
{
    if looks_like_v2(bytes) {
        match decode_v2(bytes, codec) {
            Ok(read) => return Ok(Some(read)),
            Err(e)   => { observe_decode_error("v2", &e); /* fall through */ }
        }
    }
    #[cfg(feature = "legacy-bincode1-read")]
    match decode_legacy(bytes, shape) {
        Ok(read) => return Ok(Some(read)),
        Err(e)   => observe_decode_error("legacy", &e),
    }
    Ok(None)   // unrecognised -> miss
}

fn looks_like_v2(b: &[u8]) -> bool {
    b.len() >= ENVELOPE_HEADER_LEN && b[0..3] == MAGIC && b[3] == FORMAT_V1
}
```

`decode_v2` rejects an unknown `CODEC_ID` (byte 4) with an error rather than
guessing.

For `LegacyShape::RedisOuter` the legacy check is *exact* and runs first — see
§4.5. For `LegacyShape::MemcachedOuter` the magic check is exact and runs first.
Either way, both decoders are attempted before returning a miss, so the ordering
is an optimisation, not a correctness requirement.

### 4.5 Why the dispatch cannot mis-fire (measured, not argued)

A magic prefix is only safe if no legacy buffer can begin with it. Both legacy
shapes have an *exact* structural property that rules it out `[measured]`:

**Memcached.** The legacy value begins with `CacheEntry.status` as a bincode-1
`u16` little-endian. `http::StatusCode` is constrained to `100..=999`, so
`u16::from_le_bytes([b[0], b[1]])` of any legacy memcached value is in `100..=999`.
The magic's first two bytes `"TH"` are `0x4854` = **18516**, far outside that
range. A legacy memcached value can never begin with the magic. Airtight by
construction.

**Redis.** The legacy value begins with the inner payload's length as a bincode-1
`u64` little-endian, and the outer record is exactly `payload_len + 24` bytes.
So every legacy Redis value satisfies the identity:

```
u64::from_le_bytes(b[0..8]) == b.len() - 24
```

Verified true for body sizes 0, 13, 512, 4096 and 262144 `[measured]`.

For a legacy Redis value to *begin with* the magic, its payload length would have
to be `0x01434854` = 21,120,596 bytes — a ~20 MB single cached response.
**I initially wanted to call that implausible; it is not.** `CachePolicy`'s
`max_body_size` defaults to `None`, i.e. **unbounded** (`src/policy.rs:102`) `[source]`,
so a deployment that never sets a body limit could in principle cache a 20 MB
response. The bound is therefore a probability argument, not an impossibility one,
and it should not be the only line of defence.

**So make the Redis discrimination exact rather than probabilistic.** Use the
length identity as a positive test for legacy, checked *before* the magic:

```rust
fn is_legacy_redis(b: &[u8]) -> bool {
    b.len() >= 24
        && u64::from_le_bytes(b[0..8].try_into().unwrap()) == (b.len() - 24) as u64
}
```

For `RedisBackend::get`, the dispatch order is then `is_legacy_redis` -> legacy,
else `looks_like_v2` -> v2, else miss. A v2 buffer can only satisfy
`is_legacy_redis` if it is itself ~21 MB *and* the low three bytes of its
`expires_at_ms` happen to complete the identity (a 1-in-2^24 coincidence on top of
the size requirement). Combined with the §4.4 fall-through — a mis-dispatched
attempt fails cleanly and the other decoder is tried — there is no input that
produces a wrong answer rather than a miss.

**Empirically** `[measured]`:

* 2000 legacy Redis buffers and 2000 legacy Memcached buffers, across varying body
  sizes and tag configurations, tested against `looks_like_v2`: **zero false
  positives in either set**.
* 3002 legacy Redis buffers (body sizes 0..3000 plus 100 KB and 1 MB) tested
  against `is_legacy_redis`: **3002/3002 correctly identified**.
* The same 3002 bodies encoded as v2 envelopes and tested against
  `is_legacy_redis`: **0/3002 false positives**.

### 4.6 Unrecognised bytes: miss, not error — and why

**Decision: return `Ok(None)` (a miss), and emit a `tracing::warn!` plus a
`tower_http_cache.decode_error` counter.**

Arguments for miss:

1. **The layer already does this.** `src/layer.rs:719` and `:764` read
   `if let Ok(Some(hit)) = backend.get(key_ref).await` `[source]` — an `Err` is
   *already* silently treated as a miss. Returning `Err` would add no behaviour,
   only a discarded allocation.
2. **A cache is best-effort.** An unreadable entry is semantically identical to an
   absent one. The subsequent `set` overwrites it, so the condition self-heals.
3. **Shared-namespace reality.** Operators do point two apps at one Redis DB.
   Hard-failing on a foreign key turns a cosmetic collision into an outage.
4. **Rollback.** See §4.7 — a 0.5.x binary must be able to survive 0.6.0's bytes.

Argument for error, and how it is honoured: silent data loss can mask a real
misconfiguration — two deployments sharing a namespace with different `CODEC_ID`s
would show as a 100% miss rate with no explanation. That is why the miss is
**observable**: a `warn!` (rate-limited, or logged once per distinct failure
reason) and a counter. Operators get the signal; requests get served. Do not
delete the entry on a decode failure — it may belong to another application.

### 4.7 Rollback safety (0.6.0 -> 0.5.x) — verified

If 0.6.0 writes envelope bytes and the deployment is rolled back, 0.5.1's readers
see them. Measured against real 0.5.1 code shapes `[measured]`:

| 0.5.1 reader | on v2 bytes | result |
| --- | --- | --- |
| `bincode::deserialize::<RedisRecord>` | `Err("io error: unexpected end of file")` | treated as a miss at `layer.rs:719` |
| `bincode::deserialize::<MemcachedRecord>` | `Err("invalid status code")` | treated as a miss |

**Neither returns `Ok` with garbage.** A rollback is a cold cache, not a
correctness incident. This is a direct consequence of the envelope: the codec
benchmark found that new-bytes-read-by-an-old-reader *silently succeeds with
trailing bytes ignored* on bincode 1, postcard and wincode alike when there is no
header `[measured]`. The 21-byte header is what converts that silent corruption into
a clean error. **This is the strongest argument for the envelope and belongs in
the CHANGELOG.**

### 4.8 The legacy reader must not depend on `bincode`

If the legacy reader calls `bincode::deserialize`, then `bincode 1.3.3` stays in
`Cargo.toml` through 0.6.0 and **the `deny.toml` RUSTSEC-2025-0141 ignore cannot
be deleted** — which is the entire point of the release.

**Hand-roll it instead.** The bincode-1 default configuration is a fixed,
trivially-specified encoding: little-endian fixed-width integers, `u64`
little-endian lengths for every sequence/string/byte-array, and a single `u8`
tag (0/1) for `Option`. Decoding the three fixed struct shapes needs a bounds-checked
cursor and about 80 lines.

**I implemented and verified this** `[compiled] [measured]`. A hand-rolled reader with no
`bincode` dependency decoded real `bincode 1.3.3`-produced bytes:

* Redis shape, body sizes 0 / 1 / 512 / 4096 / 262144 — **5/5 exact match** on
  status, version, headers, body, and both timestamps.
* Memcached shape with `tags = None`, `Some([])`, and
  `Some(["user:123","tenant:acme"])` — **3/3 exact match** including tags.
* 8 hostile/corrupt inputs (empty, `[0]`, all-`0xFF`, bare magic, truncated
  header, absurd declared lengths) — **0 panics**. Every length is bounds-checked
  against the remaining buffer before allocation, so a corrupt `u64` length cannot
  trigger a huge `Vec::with_capacity`.

The working reference implementation is at
`/tmp/claude-1000/-home-dcurtis-source-sadco/ea45d729-d156-4077-a480-9c23f975d262/scratchpad/envelope/src/main.rs`
(functions `Cur`, `legacy_stored_entry`, `legacy_redis`, `legacy_memcached`).
Port it, do not rewrite it from scratch.

> Note the asymmetry this preserves: the legacy **Redis** reader must set
> `tags: None` unconditionally, because 0.5.x Redis bytes genuinely do not contain
> tags (A1). The legacy **memcached** reader must read tags, because 0.5.x
> memcached bytes do contain them (A2). Getting these backwards produces a decoder
> that fails on every real entry.

### 4.9 Feature gate and the 0.7.0 removal path

```toml
[features]
default = ["in-memory", "serde", "legacy-bincode1-read"]
# Reads cache entries written by tower-http-cache 0.5.x. On by default in 0.6.0
# so upgrades do not cold-start production caches. Deprecated: this feature and
# the module behind it are removed in 0.7.0. Entries are self-expiring, so once
# every 0.5.x-written entry has aged past its TTL + stale window you can turn it
# off. Disabling it is safe at any time; it only costs a cold cache.
legacy-bincode1-read = []
```

Gate `src/codec/legacy.rs` on `#[cfg(feature = "legacy-bincode1-read")]`, and put
a `#![deprecated]`-style module doc note naming 0.7.0. Because the reader is
hand-rolled it pulls **no dependency**, so leaving it enabled costs only dead code
— the feature exists for a clean 0.7.0 deletion, not to dodge an advisory.

Removal in 0.7.0 is then a `git rm` of one module plus the feature entry.

### 4.10 Public API churn

`BincodeCodec` is exported from `src/codec.rs:16`, `src/prelude.rs:22` and named in
`src/backend/redis.rs:15` as the `RedisBackend<C = BincodeCodec>` default `[source]`.
Renaming it to `PostcardCodec` is a breaking change to a public name.

```rust
pub struct PostcardCodec;

#[deprecated(since = "0.6.0", note = "renamed to PostcardCodec; the default wire \
    format is now postcard inside a versioned envelope, not bare bincode")]
pub type BincodeCodec = PostcardCodec;
```

Keep the alias through 0.6.x, remove in 0.7.0. Note the alias makes
`RedisBackend<BincodeCodec>` keep resolving, so most downstream code is untouched.

---
## 5. Work items in detail

### W1 — Close dependabot #10 with a written reason

No code. Close `dependabot/cargo/bincode-3.0.0` and record §1.1 in the PR
close comment **and** in `CHANGELOG.md`, because the next dependabot run will
propose it again and the reason needs to be findable.

Add to `deny.toml` or a `dependabot.yml` ignore so it stops re-proposing:

```yaml
# .github/dependabot.yml
  - dependency-name: "bincode"
    versions: ["3.x"]   # 3.0.0 is a tombstone release containing only compile_error!
```

*Downstream impact:* none. *Migration note:* none.

---

### W2 — Envelope + postcard codec + unified backend record

The headline commit. **Files:** `src/codec.rs` (or split into `src/codec/mod.rs`
+ `envelope.rs`), `src/backend/redis.rs`, `src/backend/memcached.rs`, `Cargo.toml`.

1. `Cargo.toml`: add
   `postcard = { version = "1.1.3", default-features = false, features = ["use-std"], optional = true }`
   and add `dep:postcard` to the `serde` feature. **Keep `bincode` for now** — W4
   removes it, so W2 and W3 stay independently revertable.
2. `src/codec.rs`: add `tags` to `StoredEntry` (§4.3); stop routing `decode`
   through `CacheEntry::new`; swap `bincode::serialize`/`deserialize` for
   `postcard::to_allocvec` / `postcard::from_bytes`. Rename `BincodeCodec` ->
   `PostcardCodec` with the deprecated alias (§4.10).
3. New envelope module: constants, `wrap(codec_id, expires_ms, stale_ms, payload)`,
   `looks_like_v2`, `decode_v2`.
4. `src/backend/redis.rs`: delete `RedisRecord` (lines 50-55) and the
   `bincode::serialize`/`deserialize` calls at lines 68 and 103. `get` becomes
   envelope dispatch; `set` becomes `envelope::wrap(..., codec.encode(&entry)?)`.
5. `src/backend/memcached.rs`: delete `MemcachedRecord` (lines 383-388) and the
   direct `bincode` calls at 431 and 470. Add a codec type parameter mirroring
   Redis — `MemcachedBackend<C = PostcardCodec>` with `with_codec` — so **A2 is
   actually fixed** rather than just re-pointed at one hardcoded codec.
   `MemcachedBackendBuilder` needs the same parameter, or a `build_with_codec`.
6. Delete the now-meaningless `test_memcached_record_serialization`
   (`src/backend/memcached.rs:539-564`) — it constructs a record and asserts the
   fields it just set, then says in a comment that it does not test serialization.
   Replace with the real round-trip test from §7.

*Downstream impact:* **wire format changes** (mitigated by W3);
`BincodeCodec` renamed (aliased); `MemcachedBackend` gains a defaulted type
parameter, which is source-compatible for `MemcachedBackend` used as a plain type
name but breaks anyone who wrote `MemcachedBackend` in a position requiring exact
arity — rare. Anyone who implemented `CacheCodec` themselves is **unaffected**, by
design (§4.1).

*Migration note text:* see §11.1.

---

### W3 — Hand-rolled legacy reader behind `legacy-bincode1-read`

**Files:** new `src/codec/legacy.rs`, `Cargo.toml` `[features]`,
`src/backend/{redis,memcached}.rs` read paths.

Port the verified reference implementation (§4.8). Three entry points:

```rust
pub(crate) fn decode_legacy_redis(b: &[u8])     -> Result<CacheRead, CacheError>;
pub(crate) fn decode_legacy_memcached(b: &[u8]) -> Result<CacheRead, CacheError>;
```

Wire them into `read_stored` (§4.4). Add the feature per §4.9 and put it in
`default`.

**Every length read from the buffer must be bounds-checked against the remaining
buffer before it is used to allocate.** The reference `Cur::len()` does this; the
8-corrupt-input test in §7 is what proves it.

*Downstream impact:* none — additive. *Migration note:* §11.2.

---

### W4 — Remove `bincode` from `Cargo.toml`

One-line dependency removal plus the `serde` feature list. Only possible because
W3 is hand-rolled. **This is the commit that lets the RUSTSEC-2025-0141 ignore be
deleted** (§9).

Verify with `cargo tree -i bincode --all-features` returning nothing `[to run]`.

*Downstream impact:* none. *Migration note:* §11.2.

---

### W5 — `redis` 0.32.7 -> 1.6.0

**Verified: zero source changes required.** A scratch build of the real
`src/backend/redis.rs`, `src/error.rs`, both examples and `tests/redis_example.rs`
against redis 1.6.0 passed `cargo clippy --features redis-backend --all-targets -- -D warnings`
with only the version string changed `[compiled]`.

Specifically confirmed unchanged `[compiled]`: `redis::aio::ConnectionManager`
path and construction; `AsyncCommands::{get, set_ex, del}` call shapes;
`set_ex`'s TTL still `u64` seconds; the `let _: () = ...` inference pattern;
`Option<Vec<u8>>` via `FromRedisValue`; `#[from] redis::RedisError` in
`src/error.rs:11`; `Client::open`, `get_connection_manager`,
`redis::cmd("FLUSHDB").query_async::<()>`. The features `["aio", "tokio-comp",
"connection-manager"]` all still exist under those names `[metadata]`.

The documented 1.0 breaks miss this crate: safe iterators (we never iterate),
owned-`Value` `FromRedisValue` (we implement none), `async-std` removal (we use
`tokio-comp`). The one that could have bitten — `get`/`set_ex` tightening `K`/`V`
from `ToRedisArgs` to the new `ToSingleRedisArg` — passes because our key is
`String` and value is `Vec<u8>`, both of which have blanket impls `[compiled]`.

**MSRV:** redis 1.6.0 declares `rust-version = 1.88` and `edition = 2024`
`[metadata]`. That lands exactly on the existing `redis-backend` floor (already
1.88 via `url` -> `idna` -> `icu_*`). Baseline and upgrade both fail on 1.85 and
both pass on 1.88, identically `[compiled]`. **`rust-version = "1.85"` and the split
CI jobs stay correct as written — no MSRV change.**

#### W5's real risk is a runtime behaviour change, not an API break

`ConnectionManagerConfig`'s defaults changed from no timeouts to **500 ms response
/ 1 s connection** `[source]`:

```
0.32.7   DEFAULT_RESPONSE_TIMEOUT: None       DEFAULT_CONNECTION_TIMEOUT: None
1.6.0    DEFAULT_RESPONSE_TIMEOUT: Some(500ms) DEFAULT_CONNECTION_TIMEOUT: Some(1s)
```

`Client::get_connection_manager()` -> `ConnectionManager::new()` ->
`ConnectionManagerConfig::default()`, so this applies to every user of the
examples `[source]`. For an HTTP *response* cache this is a live hazard: a large
entry over a loaded or cross-AZ Redis can exceed 500 ms, and each such `get`/`set_ex`
becomes `CacheError::Redis` instead of a slow success. Because `RedisBackend::new`
takes an already-built `ConnectionManager`, the crate cannot fix this for callers
— it must be **documented**, in the `RedisBackend::new` doc comment, the README
Redis section, and both examples:

```rust
// redis 1.x defaults to a 500ms response timeout. For a response cache holding
// large bodies, raise or disable it:
let cfg = redis::aio::ConnectionManagerConfig::new()
    .set_response_timeout(None)
    .set_connection_timeout(None);
let manager = client.get_connection_manager_with_config(cfg).await?;
```

*Downstream impact:* no API break, but a **behavioural** break for anyone whose
Redis latency exceeds 500 ms. Must be in the CHANGELOG under Changed, not buried
in a dependency-bump list. *Migration note:* §11.3.

---

### W6 — Remove the redundant `Arc<Mutex<ConnectionManager>>`

Independent of W5 and independently valuable. `ConnectionManager` is
`pub struct ConnectionManager(Arc<Internals>)` with `#[derive(Clone)]` — **in
0.32.7 as well as 1.6.0** `[source]`. The `Arc<Mutex<..>>` at
`src/backend/redis.rs:16` therefore serialises every cache operation through one
lock, defeating the multiplexing the connection manager exists to provide. This is
a pre-existing performance bug, not a 1.x change.

Edits, verified compiling and clippy-clean `[compiled]`:

| line | change |
| --- | --- |
| 1 | delete `use std::sync::Arc;` |
| 8 | delete `use tokio::sync::Mutex;` |
| 16 | `connection: Arc<Mutex<ConnectionManager>>` -> `connection: ConnectionManager` |
| 24 | `connection: Arc::new(Mutex::new(connection))` -> `connection` |
| 63, 108, 114 | `let mut conn = self.connection.lock().await;` -> `let mut conn = self.connection.clone();` |

Line 39 (`with_codec`) needs no change. The field is private and `new()` still
takes a `ConnectionManager`, so there is **no public API change**. `RedisBackend`
stays `Send + Sync + Clone + 'static`.

*Downstream impact:* none (private field). Worth a CHANGELOG "Fixed" entry — it
is a concurrency fix. *Migration note:* §11.3.

---

### W7 — `bb8` 0.8 -> 0.9: un-defer this one

The brief defers #12 alongside #13. **Recommend un-deferring `bb8`, keeping
`async-memcached` deferred.** They are independent: `bb8::ManageConnection` is
implemented by *our* code (`src/backend/memcached.rs:69`), so the bb8 bump does not
touch `async-memcached` at all.

The reason to pull it in: **bb8 0.9 dropped `async-trait` for RPITIT** `[source]`.
From the v0.9.0 release notes, verbatim `[docs]`:

> bb8 0.9.0 [...] adopts RPITIT (first stabilized in Rust 1.75) to drop the
> dependency on `async_trait`. This comes at the cost of raising the MSRV for
> these new releases to 1.75.

Confirmed in source — 0.9.1 `src/api.rs:451` `[source]`:

```rust
pub trait ManageConnection: Sized + Send + Sync + 'static {
    fn connect(&self) -> impl Future<Output = Result<Self::Connection, Self::Error>> + Send;
    fn is_valid(&self, conn: &mut Self::Connection) -> impl Future<Output = Result<(), Self::Error>> + Send;
    fn has_broken(&self, conn: &mut Self::Connection) -> bool;
}
```

`async-trait` is gone from bb8 0.9.1's `[dependencies]` entirely `[metadata]`.

**`src/backend/memcached.rs:68` is the crate's *other* `#[async_trait]` use.**
While it remains, W9 cannot fully remove the `async-trait` dependency — it would
only stop using it on `CacheBackend`. So W7 is a prerequisite for W9's headline
claim.

* Latest is **bb8 0.9.1**, 2025-11-24, `rust-version = "1.75"` `[metadata]` — well
  under both MSRV tiers.
* The edit is: delete `#[async_trait]` from the `impl bb8::ManageConnection` block
  at `src/backend/memcached.rs:68`. Method bodies stay plain `async fn` `[docs]`.
* Everything else is unchanged between 0.8.6 and 0.9.1 `[source]`: identical export
  list; `Pool::builder()`, `max_size(u32)`, `min_idle(impl Into<Option<u32>>)`,
  `connection_timeout(Duration)`, `build(manager)` all same signatures;
  `Pool::state() -> State` with `connections` / `idle_connections` unchanged
  (`State` and `Statistics` are `#[non_exhaustive]` in both, so 0.9.1's added
  `get_started` stat is non-breaking). `src/backend/memcached.rs:177-183`
  (`pool_state`) needs no change.
* Risk: RPITIT checks `Send` structurally where `async_trait` boxed it. A non-`Send`
  local held across an `.await` in `connect`/`is_valid` would now fail to compile.
  Our bodies are `Client::new(&addr).await` and `conn.version().await` — no such
  local — but this must be **confirmed by compiling**, not assumed `[UNVERIFIED]`.

*Downstream impact:* anyone who wrote their own `bb8::ManageConnection` against
our re-exports. We do not re-export bb8, so impact is likely nil.
*Migration note:* §11.4.

---

### W8 — Wire the tag extractor; tell the truth about shared backends

Fixes A1b and discharges A3 honestly. **Two parts; do not skip the second.**

**W8a — make `with_tag_extractor` do something.** At both entry-construction
sites, `src/layer.rs:573` and `src/layer.rs:988`, call
`policy.extract_tags(&method, &uri)` and attach the result when non-empty:

```rust
let tags = policy.extract_tags(&method, &uri);
let entry = CacheEntry::new(status, version, headers_to_cache.unwrap(), compressed_bytes);
let entry = if tags.is_empty() { entry } else { entry.with_tags(tags) };
```

`extract_tags` already returns `Vec::new()` when `tag_policy.enabled` is false
(`src/policy.rs:188-190`) `[source]`, and `TagPolicy::enabled` defaults to `false`
(`src/tags.rs:171-175`) `[source]` — so this is **inert for every existing user who
has not opted in**. That is what keeps W8a non-breaking.

Check the refresh path at `layer.rs:573` has the request `Method`/`Uri` in scope;
if not, capture them alongside `key` when the refresh closure is built.

**W8b — the honesty fix.** `RedisBackend` and `MemcachedBackend` silently inherit
`get_keys_by_tag -> Ok(vec![])`, so `invalidate_by_tag` returns `Ok(0)` and the
caller cannot distinguish "no entries had that tag" from "this backend cannot do
tags" (§A3). Pick one:

* **Recommended:** override `get_keys_by_tag` / `list_tags` on both shared backends
  to return an explicit `Err(CacheError::Unsupported("..."))`, adding an
  `Unsupported` variant to `CacheError`. Loud, cheap, and honest. Note this makes
  the trait's defaulted `invalidate_by_tag` propagate the error, which is the
  desired behaviour.
* **Cheaper:** keep `Ok(vec![])` but add `#[doc]` on both backends and a README
  callout stating that tag invalidation is in-memory-only. Costs nothing, but a
  silent `Ok(0)` is exactly the failure mode that produced this bug report.

*Downstream impact:* W8a is inert unless `tag_policy.enabled` is set. W8b
(recommended form) changes `invalidate_by_tag` on shared backends from `Ok(0)` to
`Err` — **breaking for anyone currently calling it and ignoring the count**, which
is precisely the population being silently failed today. Call it out.
*Migration note:* §11.5.

#### A3: recommendation and cost

**Recommendation: do NOT build a backend-side tag index in 0.6.0.** Ship W2 (tags
on the wire, the prerequisite), ship W8, document the limitation, and defer the
Redis-native index to 0.7.0 behind an opt-in.

Cost estimate for the deferred work, so the decision is informed:

* **Redis: feasible, ~200-250 LOC + design.** Redis sets give the primitives:
  `SADD {ns}:tag:{tag} {key}` and `SADD {ns}:keytags:{key} {tags...}` on `set`;
  `SMEMBERS` for `get_keys_by_tag`; `SREM` on `invalidate`; `SCAN MATCH {ns}:tag:*`
  for `list_tags`. The hard part is not the commands, it is **garbage**: Redis
  expires the entry but not its set memberships, so the index grows without bound
  unless tag sets carry their own TTL and readers filter members whose key no
  longer exists. That means `get_keys_by_tag` costs an `SMEMBERS` plus an
  `EXISTS`-per-member, and `set` costs +2 round trips. Correct, but it changes the
  performance profile of every write.
* **Memcached: effectively infeasible.** Memcached has no set type. The only
  encoding is a serialized list under a tag key, and updating it is a
  read-modify-write — racy without CAS, and `async-memcached`'s CAS surface would
  need checking. Any 0.7.0 design should plan for `MemcachedBackend` to keep
  returning `Unsupported`.

So the honest 0.7.0 shape is *"opt-in Redis-native tag index; memcached
unsupported"*, which is a design decision worth making deliberately rather than
sliding into during a format release.

---
### W9 — `CacheBackend` -> RPITIT, drop `async-trait`

**Verified end to end. Full detail in §6** — the trait shape, the compile evidence,
the `+ Send` reasoning, the bb8 interaction, and the downstream break.

Summary: delete `#[async_trait]` from the trait at `src/backend/mod.rs:185` and from
all four impls (`memory.rs:39`, `redis.rs:57`, `memcached.rs:412`,
`multi_tier.rs:187`); rewrite the trait's seven methods as
`fn ..(..) -> impl Future<Output = ..> + Send`, wrapping the two defaulted bodies in
`async move { }`; add `use std::future::Future;`. Six files including `Cargo.toml`,
all inside `src/backend/`. **Zero call-site edits elsewhere** `[compiled]`.

*Downstream impact:* **breaking** for any downstream `CacheBackend` implementor.
*Migration note:* §11.7.

---

### W10 — Docs, `deny.toml`, release checklist

Cleanup pass, last because it depends on everything above landing.

1. `deny.toml`: delete the RUSTSEC-2025-0141 ignore (§9). Rewrite the comment on the
   remaining three so it no longer claims all three are fixed by one upstream
   one-liner — `fxhash` is not (§9).
2. `RELEASE_CHECKLIST.md:11`: `cargo bench` -> `cargo bench --no-run`. Also replace
   `cargo check --all-features` with `cargo test --no-run --all-features --all-targets`
   — `cargo check` is what missed the feature-matrix breakage in 0.5.1.
3. `README.md`: the five edits in §11.6.
4. `.github/workflows/ci.yml`: the `legacy-bincode1-read` on/off lines and the
   `cargo tree -i bincode` assertion from §8; fix the inaccurate memcached comment.
5. `CHANGELOG.md`: assemble §11.
6. `docs/ARCHITECTURE.md`: add the wire format as the normative reference.

*Downstream impact:* none.

---

## 6. W9 — `CacheBackend` to native `async fn` in trait (verdict: **ship it**)

### 6.1 The premise checks out

There is **no trait object anywhere**. `grep` for `dyn CacheBackend` across `src/`,
`tests/`, `examples/` and `benches/` returns nothing `[source]`; every consumer is
generic (`B: CacheBackend` at `src/layer.rs:174,310,438,507,606`, `L1`/`L2` in
`multi_tier.rs:105-106,190-191`, `AdminState<B>` at `src/admin/mod.rs:94`). The
`Clone` supertrait already made the trait non-dyn-compatible, so `async-trait` was
buying a boxed allocation per backend call and nothing else.

### 6.2 It compiles — verified, first attempt, zero workarounds

The full migration was applied to a scratch copy of HEAD and built `[compiled]`:

| check | result |
| --- | --- |
| `cargo test --no-run --all-features --all-targets` | **pass** — 5 tests, 5 examples, 1 bench all built |
| `cargo test --lib` | **118 passed, 0 failed** |
| `cargo test --test integration_cache --test cache_stale --test streaming --test auto_refresh` | **11 + 2 + 10 + 14 passed, 0 failed** |
| `cargo test --doc --all-features` | **27 passed, 1 ignored, 0 failed** |
| `--features redis-backend` / `--features memcached-backend` build | **pass** |
| `--no-default-features --features in-memory` build | **pass** |
| `cargo doc --no-deps --all-features` | **pass** |
| `cargo +1.85 check --lib` (default, and the full non-backend feature set) | **pass** |

Toolchains: `rustc 1.98.0`, plus `rustc 1.85.1` for the MSRV check, aarch64.

**MSRV 1.85 holds.** RPITIT stabilised in 1.75; nothing here needs newer.

### 6.3 The `+ Send` question, answered

The brief is right that plain `async fn` in a trait does not guarantee `Send` at
the bound site, and that `layer.rs` boxes into a `Send` future. The `+ Send` RPITIT
form is required — and it is sufficient. **`trait_variant` is not needed.**

The interesting sub-question was whether the *defaulted* methods
(`invalidate_by_tag`, `invalidate_by_tags`) survive, since they `.await` other
trait methods in a loop. **They do, with no workaround** `[compiled]` — no
`Self: Sized`, no boxing, no helper function, no extra bounds. The only mechanical
change is wrapping each body in `async move { }`, because the method is no longer
`async fn`.

This works because each method declares `+ Send` on *its own* return type, so
inside a default body the compiler already knows `Self::get_keys_by_tag(..)` and
`Self::invalidate(..)` return `Send` futures; the enclosing `async move` block then
proves `Send` without auto-trait leakage. The `Send + Sync + Clone + 'static`
supertrait supplies the rest.

RPITIT captures in-scope lifetimes implicitly, so
`fn get(&self, key: &str) -> impl Future<..> + Send` needs no lifetime annotations.

**Zero call-site edits outside `src/backend/`** `[compiled]`. `layer.rs`'s boxing,
`refresh.rs`, `admin/*`, `chunks.rs` and `multi_tier.rs`'s generic
`impl<L1: CacheBackend, L2: CacheBackend>` all compiled unchanged.

### 6.4 The trait, as it should be written

```rust
use std::future::Future;

pub trait CacheBackend: Send + Sync + Clone + 'static {
    fn get(&self, key: &str)
        -> impl Future<Output = Result<Option<CacheRead>, CacheError>> + Send;

    fn set(&self, key: String, entry: CacheEntry, ttl: Duration, stale_for: Duration)
        -> impl Future<Output = Result<(), CacheError>> + Send;

    fn invalidate(&self, key: &str)
        -> impl Future<Output = Result<(), CacheError>> + Send;

    fn get_keys_by_tag(&self, _tag: &str)
        -> impl Future<Output = Result<Vec<String>, CacheError>> + Send
    { async { Ok(Vec::new()) } }

    fn invalidate_by_tag(&self, tag: &str)
        -> impl Future<Output = Result<usize, CacheError>> + Send
    {
        async move {
            let keys = self.get_keys_by_tag(tag).await?;
            let count = keys.len();
            for key in keys { let _ = self.invalidate(&key).await; }
            Ok(count)
        }
    }

    fn invalidate_by_tags(&self, tags: &[String])
        -> impl Future<Output = Result<usize, CacheError>> + Send
    {
        async move {
            let mut total = 0;
            for tag in tags { total += self.invalidate_by_tag(tag).await?; }
            Ok(total)
        }
    }

    fn list_tags(&self)
        -> impl Future<Output = Result<Vec<String>, CacheError>> + Send
    { async { Ok(Vec::new()) } }
}
```

**Implementors are pure attribute deletions.** All four
(`memory.rs:39`, `redis.rs:57`, `memcached.rs:412`, `multi_tier.rs:187`) become:

```diff
-#[async_trait]
 impl CacheBackend for InMemoryBackend {
     async fn get(&self, key: &str) -> Result<Option<CacheRead>, CacheError> {
```

Bodies are untouched; impls keep plain `async fn` `[compiled]`.

### 6.5 The `async-trait` dependency: why W7 must land first

`src/backend/memcached.rs:68` has a **second** `#[async_trait]`, on
`impl bb8::ManageConnection`. bb8 **0.8.6** declares that trait with
`#[async_trait]` (`bb8-0.8.6/src/api.rs:384`), so removing the attribute fails
`[compiled]`:

```
error[E0195]: lifetime parameters or bounds on method `connect` do not match the trait declaration
  --> src/backend/memcached.rs:71:21
```

Two options, and they compose:

* **Without W7:** keep `async-trait` but make it optional and gate it on
  `memcached-backend`. Verified: `cargo tree -e normal` on default features then
  contains **zero** `async-trait` entries; it reappears only with
  `--features memcached-backend` `[compiled]`.

  ```diff
  -async-trait = "0.1"
  +async-trait = { version = "0.1", optional = true }
  -memcached-backend = ["async-memcached", "dep:bb8", "serde"]
  +memcached-backend = ["async-memcached", "dep:bb8", "dep:async-trait", "serde"]
  ```

* **With W7 (recommended):** bb8 0.9 moved `ManageConnection` to RPITIT and dropped
  its own `async-trait` dependency (§W7) `[source]`. Then the attribute at
  `memcached.rs:68` is deleted too and **`async-trait` leaves `Cargo.toml`
  entirely** — which is the outcome the brief actually wants.

  *This combination was not itself compiled* `[UNVERIFIED]` — the bb8 0.9 upgrade
  and the RPITIT migration were verified separately, not together. Confirm with
  `cargo test --no-run --features memcached-backend --all-targets` before claiming
  the dependency is gone.

**Do W7 before W9**, and make the "`async-trait` removed" CHANGELOG line
conditional on both landing. If W7 slips, ship the optional-and-gated form and say
so accurately.

### 6.6 Downstream impact — this is semver-major

Verified empirically. A downstream impl written with `#[async_trait]` breaks
`[compiled]`:

```
error[E0195]: lifetime parameters or bounds on method `get` do not match the trait declaration
  --> tests/downstream_async_trait.rs:12:14
```

`CacheBackend` also becomes formally non-dyn-compatible. That is irrelevant
in-crate, but a downstream `Box<dyn CacheBackend>` would break — though note the
`Clone` supertrait already prevented that from ever compiling, so the population is
almost certainly empty. See open question 9.

*Migration note text:* §11.7.

---

## 7. Test plan

### 7.1 Cross-version round-trip — the highest-risk claim in the release

"0.6.0 reads entries written by 0.5.x" is asserted, and everything else in the
release is downstream of it. It must be tested **against real 0.5.x bytes**, not
against a re-implementation of the 0.5.x encoder that shares this release's
assumptions.

**The trap to avoid:** writing the fixture with a helper that mirrors 0.5.x's
structs *from this branch*. If the mirror is wrong, the test and the reader are
wrong in the same direction and the test passes while production breaks.

**Approach: committed golden byte vectors, generated once from the real 0.5.1 code.**

1. Generate at `v0.5.1` (the tag, not this branch), with a throwaway binary that
   depends on `tower-http-cache = "=0.5.1"` from crates.io and `bincode = "1.3.3"`,
   and prints hex. Cover the matrix:

   | dimension | values |
   | --- | --- |
   | shape | Redis outer, Memcached outer |
   | body | 0, 1, 512, 4096, 262144 bytes |
   | headers | none, one, eight |
   | tags (memcached only) | `None`, `Some([])`, `Some(["user:123","tenant:acme"])` |
   | status | 200, 204, 404, 500 |
   | version | HTTP/1.0, HTTP/1.1, HTTP/2 |
   | body bytes | ASCII, and non-UTF8 `0x00..0xFF` (catches any `String`/`Vec<u8>` confusion) |

2. Commit as `tests/fixtures/v0_5_1_wire.rs` (a `const` table of
   `(&str, &[u8], ExpectedEntry)`) or as `.bin` files plus a manifest. Add a header
   comment saying how they were generated and that **they must never be
   regenerated** — regenerating them from a later version destroys their purpose.

3. New `tests/wire_compat.rs`, `required-features = ["serde"]` (it needs the codec
   but no live server — the legacy decoders operate on byte slices, so this runs in
   plain CI):

   * `legacy_redis_entries_decode` — every Redis fixture decodes to the expected
     status/version/headers/body and **`tags == None`** (0.5.x Redis genuinely had
     no tags; asserting `None` here pins A1's scope).
   * `legacy_memcached_entries_decode` — every memcached fixture decodes with
     **tags preserved** (0.5.x memcached did carry them).
   * `legacy_timestamps_survive` — `expires_at`/`stale_until` match the fixture's.
   * `v2_round_trip` — encode/decode through the new envelope for the same matrix,
     `assert_eq!` on the whole `CacheEntry` including `tags`.
   * `v2_preserves_tags_on_both_backends` — the A1 regression test. Must fail on
     `master`.
   * `magic_never_matches_legacy` — run `looks_like_v2` over every fixture and
     assert `false`. Then the property version: generate ≥2000 legacy buffers across
     body sizes and tag configurations and assert zero false positives. (I ran this;
     0/2000 and 0/2000 `[measured]`. Keep it in CI so a future magic change cannot
     silently break it.)
   * `unknown_codec_id_is_a_miss` — envelope with `CODEC_ID = 0x7E` -> `Ok(None)`,
     not `Err`, not a panic.
   * `corrupt_input_never_panics` — at minimum the 8 cases from §4.8, plus a
     fuzz-ish loop of random buffers and of valid buffers with single-byte flips.
     Assert only "no panic" and "no allocation blowup".
   * `rollback_bytes_are_rejected_by_0_5_x` — hardcode the 0.5.1 decoder shapes and
     assert v2 bytes produce `Err` (§4.7). This is the test that would catch someone
     "simplifying" the envelope away.

4. **Live-server round-trip**, `#[ignore]` by default, run manually and in the
   Redis CI job:
   * With `REDIS_URL` set: write an entry using a 0.5.1-shaped writer directly via
     `redis-cli SET` (from the committed fixture bytes), then `RedisBackend::get`
     it with 0.6.0 and assert a hit. This is the only test that exercises the real
     `GET`-returns-`Option<Vec<u8>>` path end to end.
   * The reverse: 0.6.0 writes, then assert the raw value read back with
     `redis-cli --no-raw GET` starts with `THC`.

5. **`legacy-bincode1-read` off**: assert every legacy fixture yields `Ok(None)`
   (a miss) — never an error, never a panic. Add
   `cargo test --no-default-features --features in-memory,serde,redis-backend` to
   the matrix in §8 to cover it.

### 7.2 The other bugs

* **A1b** (`tests/integration_cache.rs` or a new `tests/tags.rs`,
  `required-features = ["in-memory"]`): build a `CacheLayer` with
  `TagPolicy::new().with_enabled(true)` and a `with_tag_extractor`, drive one
  request through the service, then assert `backend.get_keys_by_tag(..)` is
  non-empty and `backend.invalidate_by_tag(..)` returns 1. **Must fail on
  `master`** — verify that before writing the fix, or the test is not testing what
  you think.
* **A1b inertness**: the same flow with the *default* `TagPolicy` (disabled) must
  leave `list_tags()` empty. This is the test that proves W8a is non-breaking.
* **A2**: a `CacheCodec` round-trip test parameterised over the codec, asserting
  that a custom codec is actually invoked by *both* `RedisBackend` and
  `MemcachedBackend`. A counting stub codec (`Arc<AtomicUsize>` bumped in
  `encode`/`decode`) proves the memcached path no longer bypasses the trait.
* **W8b**: assert `RedisBackend::invalidate_by_tag` returns the chosen sentinel
  (`Err(Unsupported)` under the recommendation).
* **`serialize_bytes` / `Vec<u8>` asymmetry**: an explicit round-trip test on
  `CacheEntry` with a non-UTF8 body. Verified passing under postcard `[compiled]`;
  the test exists so the *next* codec change cannot break it silently.

### 7.3 What not to do

* **Never `cargo bench`.** Criterion execution hung CI for 78 minutes on a sibling
  repo. `benches/cache_benchmarks.rs` must still *compile* — that is what
  `cargo test --no-run --benches` / `--all-targets` is for. `RELEASE_CHECKLIST.md`
  currently says "Benchmarks run successfully: `cargo bench`" — **change that line
  to `cargo bench --no-run`** as part of W10.
* `benches/cache_benchmarks.rs:393-420` names four `codec/bincode_*` benchmarks
  and binds `let codec = BincodeCodec;` at line 389. Rename to `codec/postcard_*`
  and `PostcardCodec` — the deprecated alias would otherwise emit a deprecation
  warning, and `-D warnings` in the clippy job would fail the build.

---

## 8. Feature-matrix verification commands

`cargo check` does not compile `#[cfg(test)]` modules, integration tests, examples
or benches. This crate has `required-features` on **5 tests, 5 examples and 1
bench**, and the 0.5.1 CHANGELOG records that a `cargo check`-based sweep missed
exactly this class of breakage. Everything below is `cargo test`-based.

```bash
# --- compile the world, including test modules, examples and benches ---
cargo test --no-run --all-features --all-targets
cargo test --no-run --features in-memory,serde,redis-backend,metrics,tracing,compression,admin-api --all-targets

# --- run what does not need a server ---
cargo test --all-features
cargo test --features in-memory,serde --doc

# --- reduced sets: these are where the feature matrix regressed twice before ---
cargo test --no-default-features --features in-memory
cargo test --no-default-features --lib --tests
cargo test --no-default-features --features serde --lib --tests

# --- NEW for 0.6.0: the legacy reader must be optional in both directions ---
cargo test --no-default-features --features in-memory,serde,redis-backend
cargo test --no-default-features --features in-memory,serde,redis-backend,legacy-bincode1-read
cargo test --no-default-features --features in-memory,serde,memcached-backend
cargo test --no-default-features --features in-memory,serde,memcached-backend,legacy-bincode1-read

# --- the advisory is only cleared if bincode is gone from every graph ---
cargo tree -i bincode --all-features        # must print "package ID not found"
cargo deny check

# --- MSRV, split as today ---
cargo +1.85.0 build
cargo +1.85.0 build --features in-memory,serde,metrics,tracing,compression,admin-api
cargo +1.88.0 build --features redis-backend
cargo +1.88.0 build --features memcached-backend

# --- lint / docs, unchanged shape ---
cargo fmt --all -- --check
cargo clippy --features in-memory,serde,redis-backend,metrics,tracing,compression,admin-api --all-targets -- -D warnings
cargo clippy --no-default-features --features in-memory -- -D warnings
cargo doc --no-deps --features in-memory,serde,redis-backend,metrics,tracing,compression,admin-api   # RUSTDOCFLAGS=-D warnings

# --- benches: COMPILE ONLY, never execute ---
cargo test --no-run --benches --features in-memory
# NEVER: cargo bench

# --- live Redis, manual / dedicated job ---
REDIS_URL=redis://127.0.0.1:6379/ cargo test --features redis-backend -- --include-ignored
```

**CI additions for `.github/workflows/ci.yml`:** the four `legacy-bincode1-read`
on/off lines and the `cargo tree -i bincode` assertion. The last is the regression
guard for §9 — without it, a future dependency could quietly reintroduce bincode
and the deleted ignore would start failing `cargo deny` for a non-obvious reason.

> **Note on `--all-features` and memcached.** The CI comment at `ci.yml`
> ("The `memcached-backend` feature is excluded from CI for this reason") is
> inaccurate: `cargo test --all-features --all-targets` includes it, and the job
> installs `libssl-dev` precisely because of it `[source]`. Either fix the comment or
> genuinely exclude the feature. Worth doing in W10 — a comment that says the
> opposite of what the job does is how the h2 advisory gets forgotten.

---

## 9. `deny.toml` — what can be deleted, what survives

`deny.toml` currently ignores four advisories.

| Advisory | 0.6.0 outcome | Why |
| --- | --- | --- |
| **RUSTSEC-2025-0141** (bincode unmaintained) | **DELETE** | W4 removes `bincode` from the graph entirely. Only possible because W3's legacy reader is hand-rolled (§4.8). Guard with `cargo tree -i bincode --all-features`. |
| **RUSTSEC-2026-0258** (h2 DoS) | **SURVIVES** | Reached only via `memcached-backend` -> `async-memcached` -> `toxiproxy_rust` -> `reqwest 0.11` -> `hyper 0.14` -> `h2`. Needs an upstream fix (§10). |
| **RUSTSEC-2025-0134** (rustls-pemfile unmaintained) | **SURVIVES** | Same path, via `reqwest 0.11`. |
| **RUSTSEC-2025-0057** (fxhash unmaintained) | **SURVIVES — and permanently, unlike the other two** | See below. |

**Correction to the working assumption.** The brief and the 0.5.1 CHANGELOG both
treat all three memcached advisories as "one test fixture, three advisories,
fixed by one upstream one-liner". That is **wrong for `fxhash`** `[metadata]`:

* `toxiproxy_rust` is genuinely misplaced. It is referenced *only* in
  `tests/resiliency_tests.rs`, where every test is
  `#[ignore = "Relies on a running memcached server and toxiproxy service"]`.
  Nothing in `src/` touches it. Moving it to `[dev-dependencies]` clears
  RUSTSEC-2026-0258 and RUSTSEC-2025-0134 for all consumers.
* **`fxhash` is a legitimate normal dependency and cannot be moved.** It is used in
  `src/lib.rs` and `src/proto/ascii_protocol.rs`, and `FxHashMap` is in
  `async-memcached`'s **public API** — `Client::stats() -> Result<FxHashMap<String, String>, Error>`
  and the `set_multi`/`*_multi` return types. Clearing RUSTSEC-2025-0057 requires
  upstream to migrate to `rustc-hash`, which is a semver-breaking API change.

Update the `deny.toml` comments accordingly — the current text promises that all
three go away together, which will mislead whoever revisits this.

**Also worth knowing:** RUSTSEC-2026-0258 has `patched = [">= 0.4.16"]`, and there
is **no 0.3.x patch** — h2 0.3.27 (2025-07-11) is the final 0.3 release `[metadata]`.
So while `hyper 0.14` is in the graph this cannot be cleared by `cargo update`. It
is remove-the-dependency or ignore.

---

## 10. Deferred: `async-memcached` 0.5 -> 0.7 (#13) — re-verified, still deferred

**The prior finding stands, re-verified against current crates.io metadata.**

* `async-memcached` **0.7.0 (2026-07-27) is still the latest**. There is no 0.7.1,
  0.7.2, or 0.8 `[metadata]`.
* `toxiproxy_rust ^0.1` and `fxhash ^0.2` are both `kind = normal` in 0.7.0 and
  0.6.0. Confirmed twice: via the crates.io `/dependencies` endpoint and by
  unpacking the `.crate` and reading `Cargo.toml.orig`, where they are the last two
  lines of `[dependencies]` `[metadata]`.
* `cargo tree` on a throwaway crate depending on `async-memcached = "0.7.0"`
  resolves **162 packages** with these inversion paths `[compiled]`:

  ```
  h2 v0.3.27       -> hyper v0.14.32 -> hyper-tls -> reqwest v0.11.27 -> toxiproxy_rust v0.1.6 -> async-memcached
  rustls-pemfile v1.0.4                            -> reqwest v0.11.27 -> toxiproxy_rust v0.1.6 -> async-memcached
  fxhash v0.2.1                                                                                 -> async-memcached
  ```

  `cargo deny check advisories` on that graph fails with **exactly the three
  claimed advisories and nothing else** `[compiled]`.
* Upstream is `Shopify/async-memcached` (not `tobz/`) `[metadata]`. `main`'s
  `Cargo.toml` is byte-identical to the published 0.7.0 manifest — **not fixed in
  git-but-unpublished**. **No existing issue or PR** mentions this `[metadata]`.
  Repo is alive but slow: last push 2026-07-29, real activity in July 2026, but
  earlier PRs sat ~5 months.
* **`toxiproxy_rust` is itself abandoned** — latest 0.1.6, published 2021-03-20,
  with no `repository` field on crates.io `[metadata]`. There is no newer version
  that drops `reqwest 0.11`, so upstream **cannot** fix this by bumping. Moving it
  to `[dev-dependencies]` is the only remedy.

**Recommendation unchanged: defer #13.** The bump costs a review of the
`AsciiProtocol` surface and gains nothing on the advisory front. `memcached-backend`
stays documented as **not recommended for production**. Revisit if 0.7.1+ ships
with the dependency moved.

**But un-defer #12 (`bb8` 0.9) — see W7.** It is independent of #13 and is a
prerequisite for dropping `async-trait`.

### 10.1 Draft upstream issue — DO NOT FILE

Drafted here per the brief. **Not filed, no comment posted, no PR opened.**
Target: `https://github.com/Shopify/async-memcached/issues/new`.

> **Title:** `toxiproxy_rust` is a normal dependency, pulling reqwest 0.11 / hyper 0.14 / h2 into every consumer
>
> **Body:**
>
> `toxiproxy_rust` is declared under `[dependencies]` rather than
> `[dev-dependencies]`, but it is only used by `tests/resiliency_tests.rs` — where
> every test is already `#[ignore = "Relies on a running memcached server and
> toxiproxy service"]`. Nothing under `src/` references it.
>
> Because it is a normal dependency it lands in the graph of every downstream
> crate, dragging in `reqwest 0.11` -> `hyper 0.14` -> `h2 0.3`, plus
> `native-tls` -> `openssl-sys`. Three consequences for consumers:
>
> 1. **A live advisory.** `RUSTSEC-2026-0258` (h2, unbounded empty DATA frames,
>    DoS) is `patched = [">= 0.4.16"]`. h2 0.3.27 is the final 0.3 release, so
>    consumers cannot resolve out of it — `cargo audit` / `cargo deny` fail with no
>    available remedy.
> 2. `RUSTSEC-2025-0134` (`rustls-pemfile` unmaintained), same path.
> 3. Building `async-memcached` now requires `libssl-dev` and `pkg-config` on the
>    build host, and duplicates the entire HTTP stack against any modern
>    `hyper 1.x` / `http 1.x` already present.
>
> `toxiproxy_rust` itself was last published in March 2021 (0.1.6) and has no
> repository link on crates.io, so there is no newer release that drops
> `reqwest 0.11`.
>
> **Proposed fix** — move the one line:
>
> ```diff
>  [dependencies]
>  ...
> -toxiproxy_rust = "0.1"
>  fxhash = "0.2"
>
>  [dev-dependencies]
>  lazy_static = "1.4"
> +toxiproxy_rust = "0.1"
>  ```
>
> The resiliency tests continue to build and run under `cargo test`; only
> downstream consumers are affected by the change.
>
> Happy to open the PR if that would help.
>
> *(Separately, and lower priority: `fxhash` is unmaintained
> (`RUSTSEC-2025-0057`) and appears in the public API via
> `Client::stats() -> Result<FxHashMap<..>, _>` and the `*_multi` return types.
> Migrating to `rustc-hash` would clear it, but it is a semver-breaking change, so
> it probably belongs in a 0.8.)*

---
## 11. Migration note text for CHANGELOG / README

Drafted ready to paste. Keep the "not a security fix" framing in 11.2 — claiming
otherwise would be false, since RUSTSEC-2025-0141 is `informational = "unmaintained"`.

### 11.1 — wire format (W2)

> ### Changed
>
> - **The on-the-wire cache format changed, and `tags` are now part of it.**
>   Entries stored in Redis and Memcached are now written as a 21-byte versioned
>   envelope (`"THC"` magic, format byte, codec byte, and the expiry/stale
>   timestamps as little-endian `u64`) followed by a `postcard`-encoded payload.
>   Previously the two backends used two *different* undocumented `bincode 1`
>   layouts.
>
>   **Upgrading is safe and does not cold-start your cache.** 0.6.0 reads
>   0.5.x-written entries transparently via the `legacy-bincode1-read` feature,
>   which is **on by default**. Entries are rewritten in the new format as they are
>   refreshed. The legacy reader is removed in 0.7.0; by then every 0.5.x entry will
>   long since have aged past its TTL.
>
>   **Rolling back to 0.5.x is also safe.** A 0.5.x binary encountering a 0.6.0
>   entry gets a clean decode error, which the cache layer already treats as a miss.
>   You get a cold cache, not corrupted responses. (This is exactly what the
>   envelope header buys: without it, an old reader would have silently accepted the
>   new bytes and ignored the trailing remainder.)
>
> - **`BincodeCodec` is renamed `PostcardCodec`.** A deprecated type alias keeps
>   `BincodeCodec` working through 0.6.x; it is removed in 0.7.0.
> - **`MemcachedBackend` now honours `CacheCodec`.** It previously called `bincode`
>   directly, bypassing the codec entirely, so `with_codec` had no memcached
>   equivalent and the two shared backends disagreed about the format. Both now
>   route through one codec and one envelope.
>
> ### Fixed
>
> - **Cache tags were silently dropped by the Redis codec.** `BincodeCodec::encode`
>   serialized a private struct with no `tags` field, and `decode` rebuilt the entry
>   through `CacheEntry::new`, which always sets `tags: None`. Tags never crossed the
>   Redis wire. They now do. (Memcached was unaffected — it serialized `CacheEntry`
>   whole. That inconsistency is also fixed.)

### 11.2 — bincode removal (W1, W3, W4)

> ### Removed
>
> - **`bincode` is no longer a dependency.** RUSTSEC-2025-0141 marked it
>   permanently unmaintained in December 2025 with no patched release, and the
>   suppression in `deny.toml` has been deleted along with the dependency.
>
>   To be precise about what this is and is not: RUSTSEC-2025-0141 is
>   `informational = "unmaintained"`, **not a vulnerability**. There is no known
>   exploit in `bincode 1.3.3`. The reason to move is that a permanently-ignored
>   advisory trains people to ignore advisories.
>
>   The reader for 0.5.x-format entries is hand-written against the (fixed, simple)
>   bincode 1 layout rather than calling `bincode`, which is what allowed the
>   dependency to be dropped in the same release that keeps backward compatibility.
>
> - **Note for anyone tracking dependabot: do not merge a `bincode 3.0` bump.**
>   `bincode` 3.0.0 is a tombstone release. Its entire `src/lib.rs` is
>   `compile_error!("https://xkcd.com/2347/");` — it was published only to signal
>   the crate's status, since crates.io has no way to archive a crate. It has no
>   features and no dependencies, and bumping to it does not compile. The last
>   functional release is 2.0.1, which is covered by the same advisory (it has no
>   version bound), so it was not a useful destination either.

### 11.3 — redis 1.x (W5, W6)

> ### Changed
>
> - **`redis` `0.32.7` -> `1.6.0`.** No API changes were needed in this crate. The
>   `redis-backend` MSRV floor is unchanged at 1.88 — redis 1.6 declares 1.88, which
>   is exactly where `url` -> `idna` -> `icu_*` already put it.
>
>   **One behavioural change to be aware of:** redis 1.x changed
>   `ConnectionManagerConfig`'s defaults from *no timeouts* to a **500 ms response
>   timeout and a 1 s connection timeout**. For a response cache holding large
>   bodies, a slow or cross-AZ Redis can exceed 500 ms, and those operations now
>   fail instead of being slow. `RedisBackend::new` takes an already-constructed
>   `ConnectionManager`, so restore the previous behaviour at construction:
>
>   ```rust
>   let cfg = redis::aio::ConnectionManagerConfig::new()
>       .set_response_timeout(None)
>       .set_connection_timeout(None);
>   let manager = client.get_connection_manager_with_config(cfg).await?;
>   let backend = RedisBackend::new(manager);
>   ```
>
> ### Fixed
>
> - **`RedisBackend` serialized every operation through a single mutex.** The
>   connection was held as `Arc<Mutex<ConnectionManager>>`, but `ConnectionManager`
>   is already `Arc`-backed, `Clone`, and internally multiplexed — the mutex
>   defeated the multiplexing it was wrapping. Removed. This affects 0.5.x as well;
>   it was not introduced by the redis upgrade.

### 11.4 — bb8 0.9 (W7)

> ### Changed
>
> - **`bb8` `0.8` -> `0.9`.** bb8 0.9 adopts RPITIT and drops its `async-trait`
>   dependency; the `impl bb8::ManageConnection` in the memcached backend no longer
>   carries `#[async_trait]`. bb8 0.9's MSRV is 1.75, below both of this crate's
>   floors, so there is no MSRV impact. This crate does not re-export `bb8`, so
>   downstream code is affected only if it implements `bb8::ManageConnection`
>   itself — in which case see bb8's own 0.9 release notes.

### 11.5 — tags (W8)

> ### Fixed
>
> - **`CachePolicy::with_tag_extractor` did nothing.** `CachePolicy::extract_tags`
>   had no callers anywhere in the crate: both places where the layer builds a
>   `CacheEntry` used `CacheEntry::new(..)` and never attached tags. Tags configured
>   through the middleware — the mechanism the README documents — never reached any
>   backend, including the in-memory one. The layer now calls `extract_tags` and
>   attaches the result.
>
>   This is inert unless you opted in: `TagPolicy::enabled` defaults to `false`, and
>   `extract_tags` returns an empty vector when it is.
>
> ### Known issues
>
> - **Tag-based invalidation works only on `InMemoryBackend` (and `MultiTierBackend`
>   over one).** `RedisBackend` and `MemcachedBackend` implement `get`, `set` and
>   `invalidate` only; they inherit the trait's default `get_keys_by_tag`, so
>   `invalidate_by_tag` has nothing to iterate. 0.6.0 puts tags *on the wire*, which
>   is a prerequisite for fixing this, but does not add a distributed tag index.
>   `TagIndex` remains process-local (`Arc<DashMap<..>>`), so even on the in-memory
>   backend, invalidating a tag clears only the calling process's index.
>
>   As of 0.6.0 the shared backends report this explicitly rather than returning a
>   silent `Ok(0)`. A Redis-native tag index (Redis sets, opt-in, with TTL-based
>   garbage collection of stale members) is planned for 0.7.0. Memcached has no set
>   type and will continue to report tags as unsupported.

### 11.6 — README edits (W10)

* README.md:142-163 ("Using Cache Tags") — currently presents
  `with_tag_extractor` + `backend.invalidate_by_tag` as a working flow on any
  backend. Add the in-memory-only caveat from 11.5.
* README.md:100-116 ("Using the Redis backend") — add the connection-timeout note
  from 11.3.
* README.md:352-364 (feature-flag table) — **`memcached-backend` is missing from
  the table entirely** `[source]`. Add it, with the production caveat. Add
  `legacy-bincode1-read`.
* README.md:366-370 (MSRV) — **says `1.75.0`, which is stale**; `Cargo.toml` says
  `1.85` with a 1.88 floor for the shared backends `[source]`. Fix, and describe the
  split. (If 0.5.2 already fixed this, skip.)
* `docs/ARCHITECTURE.md` — add the wire format (§4.2) as the normative reference.

---

### 11.7 — `CacheBackend` is no longer `#[async_trait]` (W9)

> ### Changed
>
> - **BREAKING: `CacheBackend` uses native `async fn` in traits (RPITIT) and no
>   longer depends on `async-trait`.** Every method is now declared as
>   `fn name(..) -> impl Future<Output = ..> + Send`. The `+ Send` is required
>   because the cache layer boxes backend futures into a `Send` future.
>
>   **If you implement `CacheBackend` yourself, the migration is one line per
>   impl:** delete the `#[async_trait]` attribute. Your method bodies stay exactly
>   as they are — `async fn` in an impl block is still `async fn`.
>
>   ```diff
>   -#[async_trait]
>    impl CacheBackend for MyBackend {
>        async fn get(&self, key: &str) -> Result<Option<CacheRead>, CacheError> {
>            // unchanged
>        }
>    }
>   ```
>
>   Leaving `#[async_trait]` in place produces
>   `error[E0195]: lifetime parameters or bounds on method 'get' do not match the
>   trait declaration`.
>
>   If you override the defaulted methods (`get_keys_by_tag`, `invalidate_by_tag`,
>   `invalidate_by_tags`, `list_tags`) the same one-line change applies.
>
>   This removes a boxed allocation per backend call. `CacheBackend` was already
>   non-dyn-compatible because of its `Clone` supertrait, so no working code used it
>   as a trait object; if you somehow held one, you will need a concrete type or your
>   own boxing wrapper.
>
>   MSRV is unchanged — RPITIT stabilised in Rust 1.75, well below this crate's
>   1.85 floor.

---

## 12. Open questions

Each is stated with the specific experiment that settles it. None blocks starting
W1-W6.

1. **Does the `MemcachedBackend` codec parameter break the builder ergonomically?**
   Adding `MemcachedBackend<C = PostcardCodec>` means
   `MemcachedBackendBuilder` must either gain the same parameter or grow a
   `build_with_codec`. I did not prototype it.
   *Experiment:* add the parameter in a scratch copy and run
   `cargo test --no-run --features memcached-backend --all-targets`; check
   `examples/memcached_production.rs` still compiles unchanged.

2. **Does `layer.rs:573` (the refresh path) have `Method` and `Uri` in scope for
   W8a?** The store path at `:988` is inside the service call and certainly does;
   the refresh closure may only capture `key`. `[UNVERIFIED]`
   *Experiment:* read `src/layer.rs:500-600` and try the edit; if not in scope,
   capture them when the closure is constructed at `:510`.

3. **Is `CacheError::Unsupported` the right shape for W8b, given `CacheError` is
   `#[derive(Error)]` with only two variants and no `#[non_exhaustive]`?** Adding a
   variant to a public non-`non_exhaustive` enum is a breaking change for anyone
   matching exhaustively. `[UNVERIFIED]`
   *Experiment:* decide deliberately; consider adding `#[non_exhaustive]` in the
   same release so future variants are free. That itself is breaking, so 0.6.0 is
   the right time.

4. **Does the postcard swap change the compression interaction?** `maybe_compress`
   runs *before* `CacheEntry::new` (`src/layer.rs:571`, `:983`), so the codec sees
   already-gzipped bytes. Expected to be neutral, but the ~90-byte-per-entry saving
   measured in §1.3 was on uncompressed JSON. `[UNVERIFIED]`
   *Experiment:* extend the size table with a gzip-compressed body; incompressible
   bytes should show postcard's varint saving unchanged, since the saving is in the
   length prefixes, not the payload.

5. **Throughput on the real `CacheEntry`, not the reduced struct.** Measured
   figures for the realistic entry `[measured]`: postcard encode 1425 ns/op vs
   bincode 1.3.3's 1298 (≈10% slower), postcard decode 2241 ns/op vs 3072
   (≈27% faster). Median of 9 runs, 100k iterations, `--release`, plain `Instant`
   loop, no criterion. But that was on the standalone struct on aarch64, and encode
   being slightly slower is worth confirming on the real type.
   *Experiment:* after W2, `cargo test --no-run --benches` and run the four renamed
   `codec/postcard_*` benches **manually and locally only** — never in CI.

6. **Should `wincode` be reconsidered later?** It is byte-identical to bincode 1
   output and roughly 10x faster on both paths `[measured]`, which would have made
   the migration nearly free. It is excluded now only because it declares
   `rust-version = 1.89.0` — above both the 1.85 and 1.88 tiers, verified failing on
   both `[compiled]` — and because it abandons serde (the asymmetric body helper
   would need a hand-written `SchemaWrite`/`SchemaRead`), adds 11 crates including
   `darling` and a second `syn`, and is a 0.x crate from a single vendor.
   *Experiment:* revisit when this crate's MSRV floor reaches 1.89. The envelope's
   `CODEC_ID` byte makes a later second migration cheap — which is a reason to ship
   the envelope now even if the codec choice is later revised.

7. ~~**Is `RELEASE_CHECKLIST.md` the only place `cargo bench` appears?**~~
   **Answered: no, there are three** `[source]`. `.github/workflows/ci.yml` has no
   bench step, so CI is safe today; the risk is entirely human-followed docs:
   * `RELEASE_CHECKLIST.md:11` — "Benchmarks run successfully: `cargo bench`"
   * `README.md:295` — `cargo bench --bench cache_benchmarks`
   * `README.md:401` — contributing guide asks for "benchmark output via `cargo bench`"

   Fix all three in W10. The README ones are legitimate *local* instructions, so
   annotate rather than delete: note that benchmarks are run locally and must never
   be added to CI.

8. ~~**Is the §4.5 Redis magic-collision bound safe?**~~ **Answered, and the
   answer was "no" — §4.5 has been rewritten accordingly.** `CachePolicy`'s
   `max_body_size` defaults to `None` (unbounded), `src/policy.rs:102` `[source]`, so a
   ~20 MB cached response is possible in a default configuration and a
   magic-prefix-only argument is probabilistic. §4.5 now specifies the exact
   `is_legacy_redis` length-identity test instead. No further experiment needed —
   but the `magic_never_matches_legacy` property test in §7.1 should include at
   least one ≥21 MB synthetic legacy buffer to pin this.

9. **Is anyone actually implementing `CacheBackend` downstream?** W9's breaking
   impact depends entirely on this. The crate is 0.x and pre-1.0 by its own README
   ("the public API is not yet stabilized"). `[UNVERIFIED]`
   *Experiment:* check reverse dependencies on crates.io / lib.rs for
   `tower-http-cache`. If there are none, W9's migration note can be brief.

---
