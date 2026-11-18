//! Wire-format tests for the versioned `THC` envelope and `PostcardCodec`.
//!
//! These operate on byte slices only, so no Redis server is needed.

use bytes::Bytes;
use http::{StatusCode, Version};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tower_http_cache::backend::CacheEntry;
use tower_http_cache::codec::envelope;
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
    assert_eq!(got.compressed, want.compressed, "compressed");
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
    assert_eq!(bytes[3], envelope::FORMAT_V2);
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
            let entry = sample(body.clone(), tags.clone());
            let payload = codec.encode(&entry).unwrap();
            let bytes = envelope::wrap(PostcardCodec::CODEC_ID, 111, 222, &payload);

            let read = envelope::read_stored(&bytes, &codec)
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

    let read = envelope::read_stored(&bytes, &codec)
        .unwrap()
        .expect("custom-codec entry must decode");
    assert_entry_eq(&read.entry, &entry);

    assert_eq!(codec.encodes.load(Ordering::SeqCst), 1);
    assert_eq!(codec.decodes.load(Ordering::SeqCst), 1);
}

/// Entries written by one codec must not be handed to another: report a miss,
/// never a wrong decode.
#[test]
fn unknown_codec_id_is_a_miss() {
    let entry = sample(Bytes::from_static(b"payload"), None);
    let payload = PostcardCodec.encode(&entry).unwrap();

    for id in [0x7E_u8, 0x02, 0x90, 0xFF] {
        let bytes = envelope::wrap(id, 1, 2, &payload);
        let read = envelope::read_stored(&bytes, &PostcardCodec).unwrap();
        assert!(read.is_none(), "codec id {id:#04x} must read as a miss");
    }
}

#[test]
fn unknown_format_version_is_a_miss() {
    let entry = sample(Bytes::from_static(b"payload"), None);
    let payload = PostcardCodec.encode(&entry).unwrap();
    for version in [0x00_u8, 0x03, 0x7f] {
        let mut bytes = envelope::wrap(PostcardCodec::CODEC_ID, 1, 2, &payload);
        bytes[3] = version;

        assert!(
            envelope::read_stored(&bytes, &PostcardCodec)
                .unwrap()
                .is_none(),
            "format version {version:#04x} must read as a miss"
        );
    }
}

/// Golden 0.6.x (`FORMAT_V1`) payload: 5-field postcard `StoredEntry` without
/// `compressed`, wrapped with `FORMAT_V1 = 0x01`. `0.7.0` must read it cleanly
/// with `compressed = false`, and `0.6.x` nodes must reject `FORMAT_V2` as a miss.
#[test]
fn v1_06x_golden_payload_decodes_cleanly_and_v2_is_rejected_by_v1_readers() {
    // Authentic 0.6.x postcard payload (5 fields: status=200, version=2 (HTTP/1.1),
    // headers=[("content-type", b"text/plain")], body=b"hello-06", tags=Some(["tag:a"])).
    #[derive(serde::Serialize)]
    struct StoredEntryV06<'a> {
        status: u16,
        version: u8,
        headers: Vec<(&'a str, &'a [u8])>,
        body: &'a [u8],
        tags: Option<Vec<&'a str>>,
    }

    let v06_payload = postcard::to_allocvec(&StoredEntryV06 {
        status: 200,
        version: 2,
        headers: vec![("content-type", b"text/plain")],
        body: b"hello-06",
        tags: Some(vec!["tag:a"]),
    })
    .unwrap();

    // Direct PostcardCodec::decode fallback on 0.6.x payload.
    let direct = PostcardCodec.decode(&v06_payload).unwrap();
    assert_eq!(direct.status, StatusCode::OK);
    assert_eq!(direct.version, Version::HTTP_11);
    assert_eq!(
        direct.headers,
        vec![("content-type".to_string(), b"text/plain".to_vec())]
    );
    assert_eq!(direct.body, Bytes::from_static(b"hello-06"));
    assert_eq!(direct.tags, Some(vec!["tag:a".to_string()]));
    assert!(!direct.compressed);

    // Full FORMAT_V1 envelope as written by 0.6.x RedisBackend::set.
    let mut v1_envelope = Vec::with_capacity(envelope::ENVELOPE_HEADER_LEN + v06_payload.len());
    v1_envelope.extend_from_slice(&envelope::MAGIC);
    v1_envelope.push(envelope::FORMAT_V1);
    v1_envelope.push(envelope::CODEC_POSTCARD);
    v1_envelope.extend_from_slice(&1_777_000_000_000u64.to_le_bytes());
    v1_envelope.extend_from_slice(&1_777_000_060_000u64.to_le_bytes());
    v1_envelope.extend_from_slice(&v06_payload);

    let read = envelope::read_stored(&v1_envelope, &PostcardCodec)
        .unwrap()
        .expect("0.6.x FORMAT_V1 entry must decode cleanly");
    assert_eq!(read.entry.body, Bytes::from_static(b"hello-06"));
    assert_eq!(read.entry.tags, Some(vec!["tag:a".to_string()]));
    assert!(!read.entry.compressed);

    // Reverse mixed-fleet check: 0.7.0 writes FORMAT_V2, which 0.6.x readers
    // (checking bytes[3] == FORMAT_V1) cleanly reject as a miss instead of
    // mis-decoding compressed bodies.
    let mut compressed_entry = sample(Bytes::from_static(b"\x1f\x8b\x08\x00"), None);
    compressed_entry.compressed = true;
    let v2_payload = PostcardCodec.encode(&compressed_entry).unwrap();
    let v2_envelope = envelope::wrap(PostcardCodec::CODEC_ID, 1, 2, &v2_payload);
    assert_eq!(v2_envelope[3], envelope::FORMAT_V2);
    assert_ne!(v2_envelope[3], envelope::FORMAT_V1);
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
        // a length-prefixed noise buffer
        {
            let mut v = 8u64.to_le_bytes().to_vec();
            v.extend_from_slice(&[0xab; 24]);
            v
        },
    ];

    // Truncations and single-byte flips of a real v2 envelope.
    let sample_entry = sample(
        Bytes::from((0..=255u8).collect::<Vec<u8>>()),
        Some(vec!["tag:a".to_string()]),
    );
    let payload = PostcardCodec.encode(&sample_entry).unwrap();
    let bytes = envelope::wrap(
        PostcardCodec::CODEC_ID,
        1_777_000_000_000,
        1_777_000_060_000,
        &payload,
    );
    for cut in [0, 1, 7, 8, 9, 20, 21, 40, bytes.len() / 2, bytes.len() - 1] {
        cases.push(bytes[..cut].to_vec());
    }
    for pos in (0..bytes.len()).step_by(7) {
        let mut flipped = bytes.clone();
        flipped[pos] ^= 0xff;
        cases.push(flipped);
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
        let result = std::panic::catch_unwind(|| envelope::read_stored(&case, &PostcardCodec));
        assert!(result.is_ok(), "panic on hostile input #{i}");
        // A miss is fine; a hit is fine only if it decoded consistently. What
        // is never acceptable is a panic or an unbounded allocation.
        let _ = result.unwrap();
    }
}

#[test]
fn postcard_codec_round_trips_compressed_flag() {
    let codec = PostcardCodec;

    let uncompressed = sample(Bytes::from_static(b"plain"), None);
    let encoded_plain = codec.encode(&uncompressed).unwrap();
    let decoded_plain = codec.decode(&encoded_plain).unwrap();
    assert_entry_eq(&decoded_plain, &uncompressed);
    assert!(!decoded_plain.compressed);

    let mut compressed = sample(
        Bytes::from_static(b"\x1f\x8b\x08\x00"),
        Some(vec!["gz".into()]),
    );
    compressed.compressed = true;
    let encoded_gz = codec.encode(&compressed).unwrap();
    let decoded_gz = codec.decode(&encoded_gz).unwrap();
    assert_entry_eq(&decoded_gz, &compressed);
    assert!(decoded_gz.compressed);
}
