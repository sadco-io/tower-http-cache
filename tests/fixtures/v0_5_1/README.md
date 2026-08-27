# 0.5.1 golden wire fixtures

Real bytes, produced by the **published `tower-http-cache` 0.5.1 code path** —
not by a re-implementation of it on this branch.

## Do not regenerate these files

Their entire purpose is to be bytes this branch did not write. Regenerating
them from a later version destroys that: the fixture and the reader would then
share the same assumptions, and a wrong reader would produce a passing test.
If a fixture ever fails, the reader is wrong, not the fixture.

## How they were produced

1. `tower-http-cache-0.5.1.crate` was downloaded from `static.crates.io` and
   unpacked. Its `src/codec.rs`, `src/backend/redis.rs`,
   `src/backend/memcached.rs` and `src/backend/mod.rs` are byte-identical to
   the `v0.5.1` git tag, and identical to `master` at 0.5.2 apart from an
   import reorder — so these fixtures cover both 0.5.x releases.
2. A `#[cfg(test)]` module was added *inside* `src/backend/redis.rs` and
   `src/backend/memcached.rs` of that unpacked crate, so that it could reach
   the private `RedisRecord` / `MemcachedRecord` types and 0.5.1's own
   `BincodeCodec`. Nothing about the encoding was restated by hand.
3. The module encoded the case matrix below with `bincode 1.3.3` and wrote
   each value out verbatim.

## Case matrix

Cases are named `s{status}_v{version}_h{header count}_b{body len}_{body kind}`.
Statuses 200/204/404/500; HTTP/0.9, 1.0, 1.1, 2 and 3; 0, 1 and 8 headers, one
of which carries a non-UTF8 value; bodies of 0, 1, 13, 256, 512 and 4096 bytes,
ASCII and non-UTF8; and for memcached, tags of `None`, `Some([])`,
`Some(["user:123"])` and `Some(["user:123", "tenant:acme"])`.

Redis fixtures were written from entries that *did* carry tags. They decode to
`tags: None`, because 0.5.x's Redis codec silently dropped them — that is the
A1 bug, and asserting `None` here pins its scope.

Expected field values are recomputed in `tests/wire_compat.rs` from the same
deterministic generators, so the expectation is not stored twice.

`timestamps`: case *i* has `expires_at_ms = 1_777_000_000_000 + i * 1_000` and
`stale_until_ms = expires_at_ms + 60_000`.
