//! The versioned envelope wrapped around every value written to a shared
//! backend by 0.6.0 and later.
//!
//! # Byte layout
//!
//! ```text
//!  offset  size  field             encoding                      notes
//!  ------  ----  ----------------  ----------------------------  --------------------------------
//!       0     3  MAGIC             0x54 0x48 0x43  ("THC")       constant
//!       3     1  FORMAT_VERSION    0x01                          bumped only on envelope changes
//!       4     1  CODEC_ID          0x01 = postcard               0x02..=0x7F reserved by this crate
//!                                                                0x80..=0xFF free for user codecs
//!       5     8  expires_at_ms     u64 little-endian             ms since UNIX_EPOCH
//!      13     8  stale_until_ms    u64 little-endian             ms since UNIX_EPOCH
//!      21     N  payload           CacheCodec::encode(&entry)    N = buf.len() - 21
//!  ------  ----
//!      21        ENVELOPE_HEADER_LEN
//! ```
//!
//! There is no payload length field: the transport frames the value exactly
//! (both Redis `GET` and memcached `get` return the stored length), and the
//! codec detects truncation on its own.
//!
//! Putting the timestamps in the header rather than in the codec payload keeps
//! [`CacheCodec`]'s signature free of expiry concerns, makes the timings
//! readable with `redis-cli` without running a codec, and removes the extra
//! copy of the body that the 0.5.x Redis backend spent on double-encoding.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::CacheCodec;
use crate::backend::CacheRead;
use crate::error::CacheError;

/// Magic prefix identifying a 0.6.0 envelope: `b"THC"`.
pub const MAGIC: [u8; 3] = *b"THC";

/// Envelope format version written by this release.
pub const FORMAT_V1: u8 = 0x01;

/// Codec id of [`PostcardCodec`](super::PostcardCodec).
pub const CODEC_POSTCARD: u8 = 0x01;

/// Default codec id for codecs implemented outside this crate.
pub const CODEC_USER: u8 = 0x80;

/// Size of the envelope header in bytes.
pub const ENVELOPE_HEADER_LEN: usize = 21;

/// Fixed overhead of the 0.5.x Redis outer record: an 8-byte `u64` length
/// prefix for the inner payload plus two 8-byte `u64` timestamps.
pub const LEGACY_REDIS_OVERHEAD: usize = 24;

/// Which 0.5.x layout the calling backend used, for the legacy read path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyShape {
    /// `bincode1(RedisRecord { payload: bincode1(StoredEntry), expires_at_ms, stale_until_ms })`
    RedisOuter,
    /// `bincode1(MemcachedRecord { entry: CacheEntry, expires_at_ms, stale_until_ms })`
    MemcachedOuter,
}

/// Wraps an encoded payload in an envelope header.
pub fn wrap(codec_id: u8, expires_at_ms: u64, stale_until_ms: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ENVELOPE_HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.push(FORMAT_V1);
    out.push(codec_id);
    out.extend_from_slice(&expires_at_ms.to_le_bytes());
    out.extend_from_slice(&stale_until_ms.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Reports whether `bytes` carries an envelope header this release understands.
pub fn looks_like_v2(bytes: &[u8]) -> bool {
    bytes.len() >= ENVELOPE_HEADER_LEN && bytes[0..3] == MAGIC && bytes[3] == FORMAT_V1
}

/// Exact structural test for a 0.5.x Redis value.
///
/// The 0.5.x Redis value is `bincode1(RedisRecord)`, which begins with the
/// inner payload's length as a little-endian `u64` and is exactly
/// `payload_len + 24` bytes long. Every such value therefore satisfies the
/// identity below, and it is used as a positive test for the legacy shape
/// rather than relying on the magic prefix alone: a ~21 MB cached response
/// could in principle begin with the magic bytes, and
/// [`CachePolicy::max_body_size`](crate::policy::CachePolicy) defaults to
/// unbounded.
pub fn is_legacy_redis(bytes: &[u8]) -> bool {
    bytes.len() >= LEGACY_REDIS_OVERHEAD
        && u64::from_le_bytes(bytes[0..8].try_into().unwrap())
            == (bytes.len() - LEGACY_REDIS_OVERHEAD) as u64
}

/// Decodes an envelope written by this release.
///
/// Fails if the header is absent, if the format version is unknown, or if byte
/// 4 does not name `C`'s [`CacheCodec::CODEC_ID`] — an unrecognised codec id is
/// reported rather than guessed at.
pub fn decode_v2<C: CacheCodec>(bytes: &[u8], codec: &C) -> Result<CacheRead, CacheError> {
    if bytes.len() < ENVELOPE_HEADER_LEN {
        return Err(CacheError::Backend(format!(
            "envelope too short: {} bytes",
            bytes.len()
        )));
    }
    if bytes[0..3] != MAGIC {
        return Err(CacheError::Backend("missing envelope magic".to_string()));
    }
    if bytes[3] != FORMAT_V1 {
        return Err(CacheError::Backend(format!(
            "unsupported envelope format version {:#04x}",
            bytes[3]
        )));
    }
    if bytes[4] != C::CODEC_ID {
        return Err(CacheError::Backend(format!(
            "entry was written by codec id {:#04x}, this backend uses {:#04x}",
            bytes[4],
            C::CODEC_ID
        )));
    }

    let expires_at_ms = u64::from_le_bytes(bytes[5..13].try_into().unwrap());
    let stale_until_ms = u64::from_le_bytes(bytes[13..21].try_into().unwrap());
    let entry = codec.decode(&bytes[ENVELOPE_HEADER_LEN..])?;

    Ok(CacheRead {
        entry,
        expires_at: Some(unix_ms_to_system_time(expires_at_ms)),
        stale_until: Some(unix_ms_to_system_time(stale_until_ms)),
    })
}

/// Reads a stored value, transparently accepting 0.5.x entries.
///
/// Both decoders are attempted before a miss is reported, so the dispatch
/// order is an optimisation and not a correctness requirement. Bytes that
/// neither decoder recognises are reported as `Ok(None)` — a cache miss —
/// rather than an error: the cache layer already treats a backend `Err` as a
/// miss, an unreadable entry is semantically identical to an absent one, and a
/// value that belongs to another application sharing the namespace must not
/// take a request down. The miss is observable through a `tracing::warn!` and
/// the `tower_http_cache.decode_error` counter. The value is never deleted.
pub fn read_stored<C: CacheCodec>(
    bytes: &[u8],
    codec: &C,
    shape: LegacyShape,
) -> Result<Option<CacheRead>, CacheError> {
    // For the Redis shape the legacy test is exact (see `is_legacy_redis`), so
    // it runs first when it matches. For the memcached shape the magic test is
    // exact, so the envelope runs first.
    let legacy_first = shape == LegacyShape::RedisOuter && is_legacy_redis(bytes);

    if legacy_first {
        if let Some(read) = try_legacy(bytes, shape) {
            return Ok(Some(read));
        }
    }

    if looks_like_v2(bytes) {
        match decode_v2(bytes, codec) {
            Ok(read) => return Ok(Some(read)),
            Err(err) => observe_decode_error("envelope", &err),
        }
    }

    if !legacy_first {
        if let Some(read) = try_legacy(bytes, shape) {
            return Ok(Some(read));
        }
    }

    observe_decode_error(
        "unrecognised",
        &CacheError::Backend(format!(
            "no decoder recognised the stored value ({} bytes)",
            bytes.len()
        )),
    );
    Ok(None)
}

#[cfg(feature = "legacy-bincode1-read")]
fn try_legacy(bytes: &[u8], shape: LegacyShape) -> Option<CacheRead> {
    let decoded = match shape {
        LegacyShape::RedisOuter => super::legacy::decode_legacy_redis(bytes),
        LegacyShape::MemcachedOuter => super::legacy::decode_legacy_memcached(bytes),
    };
    match decoded {
        Ok(read) => Some(read),
        Err(err) => {
            observe_decode_error("legacy-bincode1", &err);
            None
        }
    }
}

/// Without the `legacy-bincode1-read` feature, 0.5.x entries simply read as a
/// miss and are overwritten on the next store.
#[cfg(not(feature = "legacy-bincode1-read"))]
fn try_legacy(_bytes: &[u8], _shape: LegacyShape) -> Option<CacheRead> {
    None
}

fn observe_decode_error(kind: &str, err: &CacheError) {
    #[cfg(feature = "metrics")]
    metrics::counter!("tower_http_cache.decode_error", "kind" => kind.to_string()).increment(1);

    #[cfg(feature = "tracing")]
    tracing::warn!(kind = %kind, error = %err, "cache_entry_decode_failed");

    let _ = (kind, err);
}

/// Converts milliseconds since `UNIX_EPOCH` into a [`SystemTime`], the
/// encoding used by the envelope's two timestamp fields.
pub fn unix_ms_to_system_time(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
}

/// Current time in milliseconds since `UNIX_EPOCH`.
#[cfg(any(feature = "redis-backend", feature = "memcached-backend"))]
pub(crate) fn current_millis() -> Result<u64, CacheError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| CacheError::Backend(err.to_string()))?
        .as_millis() as u64)
}

/// Saturating conversion of a [`Duration`] to whole milliseconds.
#[cfg(any(feature = "redis-backend", feature = "memcached-backend"))]
pub(crate) fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}
