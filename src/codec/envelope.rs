//! The versioned envelope wrapped around every value written to a shared
//! backend by 0.6.0 and later.
//!
//! # Byte layout
//!
//! ```text
//!  offset  size  field             encoding                      notes
//!  ------  ----  ----------------  ----------------------------  --------------------------------
//!       0     3  MAGIC             0x54 0x48 0x43  ("THC")       constant
//!       3     1  FORMAT_VERSION    0x02                          0x01 (0.6.x) accepted on read;
//!                                                                0x02 written by 0.7.0+
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
//! (Redis `GET` returns the stored length), and the codec detects truncation on
//! its own.
//!
//! Putting the timestamps in the header rather than in the codec payload keeps
//! [`CacheCodec`]'s signature free of expiry concerns, makes the timings
//! readable with `redis-cli` without running a codec, and removes the extra
//! copy of the body that the 0.5.x Redis backend spent on double-encoding.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::CacheCodec;
use crate::backend::CacheRead;
use crate::error::CacheError;

/// Magic prefix identifying a `tower-http-cache` envelope: `b"THC"`.
pub const MAGIC: [u8; 3] = *b"THC";

/// Envelope format version written by 0.6.x releases (still accepted on read).
pub const FORMAT_V1: u8 = 0x01;

/// Envelope format version written by this release (adds `compressed` to the payload).
pub const FORMAT_V2: u8 = 0x02;

/// Codec id of [`PostcardCodec`](super::PostcardCodec).
pub const CODEC_POSTCARD: u8 = 0x01;

/// Default codec id for codecs implemented outside this crate.
pub const CODEC_USER: u8 = 0x80;

/// Size of the envelope header in bytes.
pub const ENVELOPE_HEADER_LEN: usize = 21;

/// Wraps an encoded payload in an envelope header.
pub fn wrap(codec_id: u8, expires_at_ms: u64, stale_until_ms: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ENVELOPE_HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.push(FORMAT_V2);
    out.push(codec_id);
    out.extend_from_slice(&expires_at_ms.to_le_bytes());
    out.extend_from_slice(&stale_until_ms.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Reports whether `bytes` carries an envelope header this release understands.
pub fn looks_like_v2(bytes: &[u8]) -> bool {
    bytes.len() >= ENVELOPE_HEADER_LEN
        && bytes[0..3] == MAGIC
        && (bytes[3] == FORMAT_V1 || bytes[3] == FORMAT_V2)
}

/// Decodes an envelope written by 0.6.x (`FORMAT_V1`) or 0.7.0+ (`FORMAT_V2`).
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
    if bytes[3] != FORMAT_V1 && bytes[3] != FORMAT_V2 {
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

/// Decodes a stored value wrapped in a versioned envelope.
///
/// Bytes that do not carry a recognised envelope or fail codec decoding are
/// reported as `Ok(None)` — a cache miss — rather than an error: the cache layer
/// already treats a backend `Err` as a miss, an unreadable entry is
/// semantically identical to an absent one, and a value that belongs to another
/// application sharing the namespace must not take a request down. The miss is
/// observable through a `tracing::warn!` and the
/// `tower_http_cache.decode_error` counter. The value is never deleted.
pub fn read_stored<C: CacheCodec>(
    bytes: &[u8],
    codec: &C,
) -> Result<Option<CacheRead>, CacheError> {
    if looks_like_v2(bytes) {
        match decode_v2(bytes, codec) {
            Ok(read) => return Ok(Some(read)),
            Err(err) => observe_decode_error("envelope", &err),
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
#[cfg(feature = "redis-backend")]
pub(crate) fn current_millis() -> Result<u64, CacheError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| CacheError::Backend(err.to_string()))?
        .as_millis() as u64)
}

/// Saturating conversion of a [`Duration`] to whole milliseconds.
#[cfg(feature = "redis-backend")]
pub(crate) fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}
