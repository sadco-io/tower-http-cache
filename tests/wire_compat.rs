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
