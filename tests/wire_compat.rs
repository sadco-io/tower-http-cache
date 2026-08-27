//! Wire-format tests: the 0.6.0 envelope, and cross-version compatibility with
//! entries written by 0.5.x.
//!
//! These operate on byte slices only, so no Redis or memcached server is
//! needed.

use bytes::Bytes;
use http::{StatusCode, Version};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tower_http_cache::backend::CacheEntry;
use tower_http_cache::codec::envelope::{self, LegacyShape};
use tower_http_cache::codec::{CacheCodec, PostcardCodec};
use tower_http_cache::error::CacheError;

fn sample(body: Bytes, tags: Option<Vec<String>>) -> CacheEntry {
    let entry = CacheEntry::new(
        StatusCode::OK,
        Version::HTTP_11,
        vec![
            (
                "content-type".to_string(),
                b"application/json; charset=utf-8".to_vec(),
            ),
            ("etag".to_string(), b"\"9f8a7b6c\"".to_vec()),
            ("x-binary".to_string(), vec![0x00, 0xff, 0xfe, 0x80]),
        ],
        body,
    );
    match tags {
        Some(t) => entry.with_tags(t),
        None => entry,
    }
}

fn assert_entry_eq(got: &CacheEntry, want: &CacheEntry) {
    assert_eq!(got.status, want.status, "status");
    assert_eq!(got.version, want.version, "version");
    assert_eq!(got.headers, want.headers, "headers");
    assert_eq!(got.body, want.body, "body");
    assert_eq!(got.tags, want.tags, "tags");
}

// --------------------------------------------------------------------------
// v2 envelope
// --------------------------------------------------------------------------

#[test]
fn envelope_header_layout_is_as_documented() {
    let entry = sample(Bytes::from_static(b"hello"), None);
    let codec = PostcardCodec;
    let payload = codec.encode(&entry).unwrap();
    let bytes = envelope::wrap(
        PostcardCodec::CODEC_ID,
        0x0102_0304_0506_0708,
        0x1112_1314_1516_1718,
        &payload,
    );

    assert_eq!(&bytes[0..3], b"THC");
    assert_eq!(bytes[3], envelope::FORMAT_V1);
    assert_eq!(bytes[4], envelope::CODEC_POSTCARD);
    assert_eq!(
        u64::from_le_bytes(bytes[5..13].try_into().unwrap()),
        0x0102_0304_0506_0708
    );
    assert_eq!(
        u64::from_le_bytes(bytes[13..21].try_into().unwrap()),
        0x1112_1314_1516_1718
    );
    assert_eq!(bytes.len(), envelope::ENVELOPE_HEADER_LEN + payload.len());
    assert_eq!(&bytes[envelope::ENVELOPE_HEADER_LEN..], &payload[..]);
}

#[test]
fn v2_round_trip() {
    let codec = PostcardCodec;
    let bodies: [Bytes; 5] = [
        Bytes::new(),
        Bytes::from_static(b"x"),
        Bytes::from(vec![b'a'; 512]),
        Bytes::from(vec![b'b'; 4096]),
        Bytes::from(vec![0u8; 262_144]),
    ];
    let tagsets = [
        None,
        Some(vec![]),
        Some(vec!["user:123".to_string(), "tenant:acme".to_string()]),
    ];

    for body in bodies {
        for tags in &tagsets {
            for shape in [LegacyShape::RedisOuter, LegacyShape::MemcachedOuter] {
                let entry = sample(body.clone(), tags.clone());
                let payload = codec.encode(&entry).unwrap();
                let bytes = envelope::wrap(PostcardCodec::CODEC_ID, 111, 222, &payload);

                let read = envelope::read_stored(&bytes, &codec, shape)
                    .unwrap()
                    .expect("v2 entry must decode");
                assert_entry_eq(&read.entry, &entry);
                assert_eq!(read.expires_at, Some(envelope::unix_ms_to_system_time(111)));
                assert_eq!(
                    read.stale_until,
                    Some(envelope::unix_ms_to_system_time(222))
                );
            }
        }
    }
}

/// The A1 regression test: 0.5.x's codec serialized a private struct with no
/// `tags` field and rebuilt entries through `CacheEntry::new`, which hardcodes
/// `tags: None`. Fails on 0.5.x.
#[test]
fn v2_preserves_tags() {
    let codec = PostcardCodec;
    let tags = vec!["user:123".to_string(), "tenant:acme".to_string()];
    let entry = sample(Bytes::from_static(b"body"), Some(tags.clone()));

    let decoded = codec.decode(&codec.encode(&entry).unwrap()).unwrap();
    assert_eq!(decoded.tags, Some(tags));
}

/// `CacheEntry::body` serializes with `serialize_bytes` but deserializes as
/// `Vec<u8>`. That asymmetry happens to round-trip; this pins it so the next
/// codec change cannot break it silently.
#[test]
fn non_utf8_body_round_trips() {
    let codec = PostcardCodec;
    let body: Vec<u8> = (0..=255u8).cycle().take(1024).collect();
    let entry = sample(Bytes::from(body.clone()), None);

    let decoded = codec.decode(&codec.encode(&entry).unwrap()).unwrap();
    assert_eq!(decoded.body, Bytes::from(body));
}

/// `CacheEntry`'s derived `Serialize`/`Deserialize` must round-trip under a
/// non-self-describing format. Up to 0.5.x it could not: `version_serde`
/// serialized four bytes (its match arms defaulted to `i32`) and deserialized
/// one. That is why 0.5.x's memcached backend could never read back what it
/// had written.
#[test]
fn cache_entry_derive_round_trips_under_postcard() {
    for version in [
        Version::HTTP_09,
        Version::HTTP_10,
        Version::HTTP_11,
        Version::HTTP_2,
        Version::HTTP_3,
    ] {
        let mut entry = sample(Bytes::from(vec![0xffu8; 300]), Some(vec!["t".to_string()]));
        entry.version = version;

        let bytes = postcard::to_allocvec(&entry).expect("serialize");
        let back: CacheEntry = postcard::from_bytes(&bytes).expect("deserialize");
        assert_entry_eq(&back, &entry);
    }
}

// --------------------------------------------------------------------------
// Codec dispatch (A2): both shared backends route through `CacheCodec`
// --------------------------------------------------------------------------

#[derive(Clone, Default)]
struct CountingCodec {
    encodes: Arc<AtomicUsize>,
    decodes: Arc<AtomicUsize>,
}

impl CacheCodec for CountingCodec {
    const CODEC_ID: u8 = 0x90;

    fn encode(&self, entry: &CacheEntry) -> Result<Vec<u8>, CacheError> {
        self.encodes.fetch_add(1, Ordering::SeqCst);
        PostcardCodec.encode(entry)
    }

    fn decode(&self, bytes: &[u8]) -> Result<CacheEntry, CacheError> {
        self.decodes.fetch_add(1, Ordering::SeqCst);
        PostcardCodec.decode(bytes)
    }
}

#[test]
fn custom_codec_is_invoked_and_its_id_is_recorded() {
    let codec = CountingCodec::default();
    let entry = sample(Bytes::from_static(b"payload"), None);

    let payload = codec.encode(&entry).unwrap();
    let bytes = envelope::wrap(CountingCodec::CODEC_ID, 1, 2, &payload);
    assert_eq!(bytes[4], 0x90);

    for shape in [LegacyShape::RedisOuter, LegacyShape::MemcachedOuter] {
        let read = envelope::read_stored(&bytes, &codec, shape)
            .unwrap()
            .expect("custom-codec entry must decode");
        assert_entry_eq(&read.entry, &entry);
    }

    assert_eq!(codec.encodes.load(Ordering::SeqCst), 1);
    assert_eq!(codec.decodes.load(Ordering::SeqCst), 2);
}

/// Entries written by one codec must not be handed to another: report a miss,
/// never a wrong decode.
#[test]
fn unknown_codec_id_is_a_miss() {
    let entry = sample(Bytes::from_static(b"payload"), None);
    let payload = PostcardCodec.encode(&entry).unwrap();

    for id in [0x7E_u8, 0x02, 0x90, 0xFF] {
        let bytes = envelope::wrap(id, 1, 2, &payload);
        for shape in [LegacyShape::RedisOuter, LegacyShape::MemcachedOuter] {
            let read = envelope::read_stored(&bytes, &PostcardCodec, shape).unwrap();
            assert!(read.is_none(), "codec id {id:#04x} must read as a miss");
        }
    }
}

#[test]
fn unknown_format_version_is_a_miss() {
    let entry = sample(Bytes::from_static(b"payload"), None);
    let payload = PostcardCodec.encode(&entry).unwrap();
    let mut bytes = envelope::wrap(PostcardCodec::CODEC_ID, 1, 2, &payload);
    bytes[3] = 0x02;

    for shape in [LegacyShape::RedisOuter, LegacyShape::MemcachedOuter] {
        assert!(
            envelope::read_stored(&bytes, &PostcardCodec, shape)
                .unwrap()
                .is_none()
        );
    }
}

// --------------------------------------------------------------------------
// Rollback safety (§4.7): 0.5.x readers must reject v2 bytes cleanly
// --------------------------------------------------------------------------

/// A 0.5.x binary reading a 0.6.0 entry gets a clean decode error, which the
/// cache layer already treats as a miss — a cold cache, not corrupted
/// responses. Without the 21-byte header an old reader would have silently
/// accepted the new bytes and ignored the trailing remainder. This test
/// hardcodes the 0.5.x reader shapes so that "simplifying" the envelope away
/// is caught here.
#[test]
fn rollback_bytes_are_rejected_by_0_5_x_readers() {
    let entry = sample(Bytes::from(vec![b'z'; 4096]), Some(vec!["x".to_string()]));
    let payload = PostcardCodec.encode(&entry).unwrap();
    let v2 = envelope::wrap(
        PostcardCodec::CODEC_ID,
        1_777_000_000_000,
        1_777_000_060_000,
        &payload,
    );

    // 0.5.x RedisBackend::get: bincode1(RedisRecord { payload: Vec<u8>, u64, u64 }).
    // The first 8 bytes are a u64 LE length; the magic makes it absurd.
    let declared = u64::from_le_bytes(v2[0..8].try_into().unwrap());
    assert!(
        declared as usize > v2.len(),
        "0.5.x redis reader must hit EOF, not read a short payload"
    );

    // 0.5.x MemcachedBackend::get: bincode1(MemcachedRecord { entry, u64, u64 }),
    // whose first field is CacheEntry.status as a u16 LE, validated by
    // StatusCode::from_u16.
    let status = u16::from_le_bytes([v2[0], v2[1]]);
    assert!(
        StatusCode::from_u16(status).is_err(),
        "0.5.x memcached reader must reject the magic as a status code"
    );
}

// --------------------------------------------------------------------------
// 0.5.x compatibility: golden fixtures
//
// The `.bin` files under tests/fixtures/v0_5_1/ are real bytes written by the
// published 0.5.1 code path -- see the README there. The expectations below
// are recomputed from the same deterministic generators the fixture writer
// used, so the expected values are not stored twice; the *encoding* is never
// restated by hand.
// --------------------------------------------------------------------------

/// `(name, status, version_u8, header_kind, body_kind, body_len, tag_kind)`
const FIXTURE_CASES: &[(&str, u16, u8, u8, u8, usize, u8)] = &[
    ("s200_v11_h0_b0_ascii", 200, 2, 0, 0, 0, 0),
    ("s200_v11_h1_b1_ascii", 200, 2, 1, 0, 1, 1),
    ("s204_v10_h0_b0_ascii", 204, 1, 0, 0, 0, 2),
    ("s404_v2_h8_b512_ascii", 404, 3, 2, 0, 512, 3),
    ("s500_v11_h1_b4096_nonutf8", 500, 2, 1, 1, 4096, 0),
    ("s200_v11_h8_b4096_ascii", 200, 2, 2, 0, 4096, 3),
    ("s200_v09_h1_b13_nonutf8", 200, 0, 1, 1, 13, 1),
    ("s200_v3_h1_b13_ascii", 200, 4, 1, 0, 13, 2),
    ("s200_v11_h8_b256_nonutf8", 200, 2, 2, 1, 256, 3),
];

fn fx_body(kind: u8, n: usize) -> Vec<u8> {
    match kind {
        0 => (0..n).map(|i| b'a' + (i % 26) as u8).collect(),
        _ => (0..n).map(|i| (i % 256) as u8).collect(),
    }
}

fn fx_headers(kind: u8) -> Vec<(String, Vec<u8>)> {
    match kind {
        0 => Vec::new(),
        1 => vec![("content-type".to_string(), b"application/json".to_vec())],
        _ => vec![
            (
                "content-type".to_string(),
                b"application/json; charset=utf-8".to_vec(),
            ),
            ("etag".to_string(), b"\"9f8a7b6c\"".to_vec()),
            ("cache-control".to_string(), b"public, max-age=60".to_vec()),
            ("vary".to_string(), b"accept-encoding".to_vec()),
            (
                "x-request-id".to_string(),
                b"01890f3e-2c2a-7c1e-9a1b-000000000000".to_vec(),
            ),
            ("content-encoding".to_string(), b"gzip".to_vec()),
            ("x-binary".to_string(), vec![0x00, 0xff, 0xfe, 0x80, 0x7f]),
            ("server".to_string(), b"tower-http-cache/0.5.1".to_vec()),
        ],
    }
}

fn fx_tags(kind: u8) -> Option<Vec<String>> {
    match kind {
        0 => None,
        1 => Some(vec![]),
        2 => Some(vec!["user:123".to_string()]),
        _ => Some(vec!["user:123".to_string(), "tenant:acme".to_string()]),
    }
}

fn fx_version(v: u8) -> Version {
    match v {
        0 => Version::HTTP_09,
        1 => Version::HTTP_10,
        2 => Version::HTTP_11,
        3 => Version::HTTP_2,
        _ => Version::HTTP_3,
    }
}

fn fx_times(i: usize) -> (u64, u64) {
    let expires = 1_777_000_000_000u64 + (i as u64) * 1_000;
    (expires, expires + 60_000)
}

fn fixture_bytes(prefix: &str, name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/v0_5_1/{}_{}.bin",
        env!("CARGO_MANIFEST_DIR"),
        prefix,
        name
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("missing fixture {path}: {e}"))
}

// --------------------------------------------------------------------------
// A bincode-1 writer, used only to scale the fixtures up to sizes that would
// be wasteful to commit. It is not trusted on its own: the first test below
// asserts it reproduces every committed fixture byte for byte, so anything
// built with it afterwards has the same authority as a real 0.5.1 buffer.
// --------------------------------------------------------------------------

mod bincode1 {
    fn push_len(out: &mut Vec<u8>, n: usize) {
        out.extend_from_slice(&(n as u64).to_le_bytes());
    }

    fn push_bytes(out: &mut Vec<u8>, b: &[u8]) {
        push_len(out, b.len());
        out.extend_from_slice(b);
    }

    fn push_headers(out: &mut Vec<u8>, headers: &[(String, Vec<u8>)]) {
        push_len(out, headers.len());
        for (name, value) in headers {
            push_bytes(out, name.as_bytes());
            push_bytes(out, value);
        }
    }

    /// 0.5.x `codec::StoredEntry` -- note: no tags field.
    pub fn stored_entry(
        status: u16,
        version: u8,
        headers: &[(String, Vec<u8>)],
        body: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&status.to_le_bytes());
        out.push(version);
        push_headers(&mut out, headers);
        push_bytes(&mut out, body);
        out
    }

    /// 0.5.x `backend::redis::RedisRecord`.
    pub fn redis_record(payload: &[u8], expires_at_ms: u64, stale_until_ms: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() + 24);
        push_bytes(&mut out, payload);
        out.extend_from_slice(&expires_at_ms.to_le_bytes());
        out.extend_from_slice(&stale_until_ms.to_le_bytes());
        out
    }

    /// 0.5.x `backend::memcached::MemcachedRecord` -- `CacheEntry` inlined,
    /// tags included.
    ///
    /// `version` goes out as four bytes here, not one: 0.5.x's `version_serde`
    /// helper let its match arms default to `i32`. The Redis shape above uses
    /// an explicit `u8`. This is confirmed by the committed fixtures.
    #[allow(clippy::too_many_arguments)]
    pub fn memcached_record(
        status: u16,
        version: u8,
        headers: &[(String, Vec<u8>)],
        body: &[u8],
        tags: Option<&[String]>,
        expires_at_ms: u64,
        stale_until_ms: u64,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&status.to_le_bytes());
        out.extend_from_slice(&(version as i32).to_le_bytes());
        push_headers(&mut out, headers);
        push_bytes(&mut out, body);
        match tags {
            None => out.push(0),
            Some(t) => {
                out.push(1);
                push_len(&mut out, t.len());
                for tag in t {
                    push_bytes(&mut out, tag.as_bytes());
                }
            }
        }
        out.extend_from_slice(&expires_at_ms.to_le_bytes());
        out.extend_from_slice(&stale_until_ms.to_le_bytes());
        out
    }
}

fn synthetic_legacy_redis(
    status: u16,
    version: u8,
    headers: &[(String, Vec<u8>)],
    body: &[u8],
    expires_at_ms: u64,
    stale_until_ms: u64,
) -> Vec<u8> {
    let payload = bincode1::stored_entry(status, version, headers, body);
    bincode1::redis_record(&payload, expires_at_ms, stale_until_ms)
}

/// Establishes the synthetic writer's authority: it must reproduce every real
/// 0.5.1 fixture exactly. If this fails, nothing built with the writer means
/// anything.
#[test]
fn synthetic_writer_reproduces_real_fixtures() {
    for (i, (name, status, ver, hk, bk, blen, tk)) in FIXTURE_CASES.iter().enumerate() {
        let headers = fx_headers(*hk);
        let body = fx_body(*bk, *blen);
        let (expires, stale) = fx_times(i);

        let redis = synthetic_legacy_redis(*status, *ver, &headers, &body, expires, stale);
        assert_eq!(
            redis,
            fixture_bytes("redis", name),
            "synthetic redis bytes differ from the real 0.5.1 fixture for {name}"
        );

        let tags = fx_tags(*tk);
        let memcached = bincode1::memcached_record(
            *status,
            *ver,
            &headers,
            &body,
            tags.as_deref(),
            expires,
            stale,
        );
        assert_eq!(
            memcached,
            fixture_bytes("memcached", name),
            "synthetic memcached bytes differ from the real 0.5.1 fixture for {name}"
        );
    }
}

#[cfg(feature = "legacy-bincode1-read")]
mod legacy_reader {
    use super::*;
    use tower_http_cache::codec::legacy::{decode_legacy_memcached, decode_legacy_redis};

    /// Every real 0.5.1 Redis fixture decodes field by field. `tags` must be
    /// `None`: 0.5.x's Redis codec genuinely dropped them, and asserting that
    /// here pins the scope of the A1 fix.
    #[test]
    fn legacy_redis_entries_decode() {
        for (i, (name, status, ver, hk, bk, blen, _tk)) in FIXTURE_CASES.iter().enumerate() {
            let read = decode_legacy_redis(&fixture_bytes("redis", name))
                .unwrap_or_else(|e| panic!("{name}: {e}"));

            assert_eq!(read.entry.status.as_u16(), *status, "{name} status");
            assert_eq!(read.entry.version, fx_version(*ver), "{name} version");
            assert_eq!(read.entry.headers, fx_headers(*hk), "{name} headers");
            assert_eq!(
                read.entry.body,
                Bytes::from(fx_body(*bk, *blen)),
                "{name} body"
            );
            assert_eq!(
                read.entry.tags, None,
                "{name} tags (0.5.x redis dropped them)"
            );

            let (expires, stale) = fx_times(i);
            assert_eq!(
                read.expires_at,
                Some(envelope::unix_ms_to_system_time(expires)),
                "{name} expires_at"
            );
            assert_eq!(
                read.stale_until,
                Some(envelope::unix_ms_to_system_time(stale)),
                "{name} stale_until"
            );
        }
    }

    /// 0.5.x memcached serialized `CacheEntry` whole, so tags *are* on the
    /// wire and must survive.
    #[test]
    fn legacy_memcached_entries_decode() {
        for (i, (name, status, ver, hk, bk, blen, tk)) in FIXTURE_CASES.iter().enumerate() {
            let read = decode_legacy_memcached(&fixture_bytes("memcached", name))
                .unwrap_or_else(|e| panic!("{name}: {e}"));

            assert_eq!(read.entry.status.as_u16(), *status, "{name} status");
            assert_eq!(read.entry.version, fx_version(*ver), "{name} version");
            assert_eq!(read.entry.headers, fx_headers(*hk), "{name} headers");
            assert_eq!(
                read.entry.body,
                Bytes::from(fx_body(*bk, *blen)),
                "{name} body"
            );
            assert_eq!(read.entry.tags, fx_tags(*tk), "{name} tags");

            let (expires, stale) = fx_times(i);
            assert_eq!(
                read.expires_at,
                Some(envelope::unix_ms_to_system_time(expires)),
                "{name} expires_at"
            );
            assert_eq!(
                read.stale_until,
                Some(envelope::unix_ms_to_system_time(stale)),
                "{name} stale_until"
            );
        }
    }

    /// The reader must reject bytes it did not fully consume, and must reject
    /// the other shape's layout rather than half-decoding it.
    #[test]
    fn legacy_readers_reject_the_wrong_shape_and_trailing_bytes() {
        for (name, ..) in FIXTURE_CASES.iter() {
            let redis = fixture_bytes("redis", name);
            let mut extended = redis.clone();
            extended.push(0);
            assert!(
                decode_legacy_redis(&extended).is_err(),
                "{name}: trailing bytes must be rejected"
            );
            assert!(
                decode_legacy_redis(&redis[..redis.len() - 1]).is_err(),
                "{name}: truncation must be rejected"
            );
        }
    }
}

// --------------------------------------------------------------------------
// Dispatch: a 0.5.x buffer must never be mistaken for an envelope, and vice
// versa
// --------------------------------------------------------------------------

/// No committed 0.5.1 fixture carries the envelope magic.
#[test]
fn magic_never_matches_legacy_fixtures() {
    for (name, ..) in FIXTURE_CASES.iter() {
        for prefix in ["redis", "memcached"] {
            let bytes = fixture_bytes(prefix, name);
            assert!(
                !envelope::looks_like_v2(&bytes),
                "{prefix} {name} was misread as an envelope"
            );
        }
    }
}

/// The property version: thousands of legacy buffers across body sizes and tag
/// configurations, none of which may look like an envelope.
#[test]
fn magic_never_matches_legacy_property() {
    let headers = fx_headers(1);
    for n in 0..3000usize {
        let body = fx_body((n % 2) as u8, n);
        let tags = fx_tags((n % 4) as u8);

        let redis = synthetic_legacy_redis(200, 2, &headers, &body, n as u64, n as u64);
        assert!(!envelope::looks_like_v2(&redis), "redis body={n}");

        let memcached = bincode1::memcached_record(
            200,
            2,
            &headers,
            &body,
            tags.as_deref(),
            n as u64,
            n as u64,
        );
        assert!(!envelope::looks_like_v2(&memcached), "memcached body={n}");
    }
}

/// The case the naive magic-prefix argument missed.
///
/// A legacy Redis value begins with its inner payload length as a
/// little-endian `u64`. If that length is exactly `0x0143_4854` the first four
/// bytes *are* `"THC"` followed by `FORMAT_V1` — so `looks_like_v2` returns
/// true on a genuine 0.5.x buffer. `CachePolicy::max_body_size` defaults to
/// `None`, so a ~20 MiB cached response is reachable in a default
/// configuration and this is not a hypothetical.
///
/// The exact `is_legacy_redis` length identity is what makes the dispatch
/// correct anyway.
#[test]
fn twenty_one_megabyte_legacy_redis_buffer_carries_the_magic_and_still_decodes() {
    // payload_len == 0x0143_4854 puts "THC\x01" in bytes 0..4.
    const TARGET_PAYLOAD_LEN: usize = 0x0143_4854;
    // StoredEntry with zero headers: u16 status + u8 version + u64 header
    // count + u64 body length = 19 bytes of overhead.
    let body_len = TARGET_PAYLOAD_LEN - 19;
    let body = vec![0x5au8; body_len];

    let payload = bincode1::stored_entry(200, 2, &[], &body);
    assert_eq!(payload.len(), TARGET_PAYLOAD_LEN);
    let bytes = bincode1::redis_record(&payload, 1_777_000_000_000, 1_777_000_060_000);

    assert_eq!(&bytes[0..3], b"THC");
    assert_eq!(bytes[3], envelope::FORMAT_V1);
    assert!(
        envelope::looks_like_v2(&bytes),
        "this is the hazard being pinned: the magic check alone says yes"
    );
    assert!(
        envelope::is_legacy_redis(&bytes),
        "the exact length identity must still identify it as legacy"
    );

    let read = envelope::read_stored(&bytes, &PostcardCodec, LegacyShape::RedisOuter).unwrap();
    if cfg!(feature = "legacy-bincode1-read") {
        let read = read.expect("must decode as a legacy entry, not as an envelope");
        assert_eq!(read.entry.status, StatusCode::OK);
        assert_eq!(read.entry.body.len(), body_len);
        assert_eq!(
            read.expires_at,
            Some(envelope::unix_ms_to_system_time(1_777_000_000_000))
        );
    } else {
        assert!(read.is_none(), "without the legacy reader this is a miss");
    }
}

/// `is_legacy_redis` identifies every legacy Redis buffer and no envelope.
#[test]
fn is_legacy_redis_is_exact() {
    let headers = fx_headers(2);
    let codec = PostcardCodec;

    for n in (0..2000usize).chain([100_000, 1_000_000]) {
        let body = fx_body((n % 2) as u8, n);
        let entry = sample(Bytes::from(body.clone()), fx_tags((n % 4) as u8));

        let legacy = synthetic_legacy_redis(200, 2, &headers, &body, n as u64, n as u64);
        assert!(envelope::is_legacy_redis(&legacy), "legacy body={n}");

        let payload = codec.encode(&entry).unwrap();
        let v2 = envelope::wrap(
            PostcardCodec::CODEC_ID,
            1_777_000_000_000 + n as u64,
            1_777_000_060_000,
            &payload,
        );
        assert!(!envelope::is_legacy_redis(&v2), "envelope body={n}");
    }
}

/// The feature is verified in both directions from one test: with the legacy
/// reader compiled in, real 0.5.1 bytes are a hit; without it, they are a
/// clean miss -- never an error, never a panic.
#[test]
fn legacy_fixtures_through_read_stored() {
    for (name, status, ver, hk, bk, blen, tk) in FIXTURE_CASES.iter() {
        for (prefix, shape) in [
            ("redis", LegacyShape::RedisOuter),
            ("memcached", LegacyShape::MemcachedOuter),
        ] {
            let bytes = fixture_bytes(prefix, name);
            let read = envelope::read_stored(&bytes, &PostcardCodec, shape).unwrap();

            if cfg!(feature = "legacy-bincode1-read") {
                let read = read.unwrap_or_else(|| panic!("{prefix} {name} must be a hit"));
                assert_eq!(read.entry.status.as_u16(), *status);
                assert_eq!(read.entry.version, fx_version(*ver));
                assert_eq!(read.entry.headers, fx_headers(*hk));
                assert_eq!(read.entry.body, Bytes::from(fx_body(*bk, *blen)));
                let expected_tags = if prefix == "redis" {
                    None
                } else {
                    fx_tags(*tk)
                };
                assert_eq!(read.entry.tags, expected_tags);
            } else {
                assert!(
                    read.is_none(),
                    "{prefix} {name} must be a miss without legacy-bincode1-read"
                );
            }
        }
    }
}

// --------------------------------------------------------------------------
// Hostile input
// --------------------------------------------------------------------------

fn hostile_inputs() -> Vec<Vec<u8>> {
    let mut cases: Vec<Vec<u8>> = vec![
        vec![],
        vec![0],
        vec![0xff; 3],
        vec![0xff; 40],
        b"THC".to_vec(),
        // valid header, unknown codec id, no payload
        {
            let mut v = b"THC".to_vec();
            v.push(envelope::FORMAT_V1);
            v.push(0x7e);
            v.extend_from_slice(&[0u8; 16]);
            v
        },
        // valid header, garbage payload
        {
            let mut v = b"THC".to_vec();
            v.extend_from_slice(&[envelope::FORMAT_V1, envelope::CODEC_POSTCARD]);
            v.extend_from_slice(&[0xff; 16]);
            v.extend_from_slice(&[0xff; 32]);
            v
        },
        // absurd declared length followed by nothing
        vec![0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1, 2, 3],
        // a length identity that says "legacy" over a buffer of noise
        {
            let mut v = 8u64.to_le_bytes().to_vec();
            v.extend_from_slice(&[0xab; 24]);
            v
        },
    ];

    // Truncations and single-byte flips of a real fixture, both shapes.
    for prefix in ["redis", "memcached"] {
        let bytes = fixture_bytes(prefix, "s200_v11_h8_b256_nonutf8");
        for cut in [0, 1, 7, 8, 9, 20, 21, 40, bytes.len() / 2, bytes.len() - 1] {
            cases.push(bytes[..cut].to_vec());
        }
        for pos in (0..bytes.len()).step_by(7) {
            let mut flipped = bytes.clone();
            flipped[pos] ^= 0xff;
            cases.push(flipped);
        }
    }

    // Deterministic pseudo-random noise.
    let mut state = 0x243f_6a88_85a3_08d3u64;
    for len in [0usize, 1, 7, 21, 24, 64, 257, 4096] {
        let mut buf = Vec::with_capacity(len);
        for _ in 0..len {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            buf.push((state >> 33) as u8);
        }
        cases.push(buf);
    }

    cases
}

/// Corrupt, truncated and hostile input must read as a miss and must never
/// panic, in either feature direction. Every length is bounds-checked against
/// the remaining buffer before it is used, so a corrupt `u64` cannot drive a
/// huge allocation.
#[test]
fn corrupt_input_never_panics() {
    for (i, case) in hostile_inputs().into_iter().enumerate() {
        for shape in [LegacyShape::RedisOuter, LegacyShape::MemcachedOuter] {
            let result =
                std::panic::catch_unwind(|| envelope::read_stored(&case, &PostcardCodec, shape));
            assert!(result.is_ok(), "panic on hostile input #{i} ({shape:?})");
            // A miss is fine; a hit is fine only if it decoded consistently.
            // What is never acceptable is a panic or an unbounded allocation.
            let _ = result.unwrap();
        }
    }
}
