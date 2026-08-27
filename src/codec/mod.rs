//! Serialization of cached entries, and the on-the-wire format used by the
//! shared backends.
//!
//! Two layers are involved, and they are deliberately separate:
//!
//! * A [`CacheCodec`] turns a [`CacheEntry`] into bytes and back. The default
//!   is [`PostcardCodec`]. A codec knows nothing about expiry.
//! * The [`envelope`] wraps that payload in a 21-byte versioned header that
//!   carries the magic, the format version, the codec id, and the entry's
//!   expiry and stale-until timestamps.
//!
//! See [`envelope`] for the byte layout. Custom codecs only implement
//! [`CacheCodec`]; the envelope is applied by the backend.

pub mod envelope;
#[cfg(feature = "legacy-bincode1-read")]
pub mod legacy;

use bytes::Bytes;
use http::{StatusCode, Version};
use serde::{Deserialize, Serialize};

use crate::backend::CacheEntry;
use crate::error::CacheError;

/// Trait representing a serialization strategy for cached entries.
///
/// The payload produced by [`encode`](CacheCodec::encode) is stored inside the
/// [`envelope`], which supplies the timing metadata. Implementations therefore
/// only need to round-trip the entry itself.
pub trait CacheCodec: Send + Sync + Clone + 'static {
    /// Identifies this codec in byte 4 of the [`envelope`] header.
    ///
    /// Entries are only decoded by a codec whose `CODEC_ID` matches the byte
    /// recorded when they were written; a mismatch is reported as a miss rather
    /// than decoded with the wrong codec.
    ///
    /// `0x00..=0x7F` is reserved for this crate (`0x01` is
    /// [`PostcardCodec`]). `0x80..=0xFF` is free for downstream codecs, and
    /// the default value is [`envelope::CODEC_USER`] (`0x80`).
    const CODEC_ID: u8 = envelope::CODEC_USER;

    fn encode(&self, entry: &CacheEntry) -> Result<Vec<u8>, CacheError>;
    fn decode(&self, bytes: &[u8]) -> Result<CacheEntry, CacheError>;
}

/// Default [`CacheCodec`] implementation, backed by [`postcard`].
///
/// Replaces the `bincode`-backed codec used up to 0.5.x. The payload carries
/// `tags`, which the previous codec silently dropped.
#[derive(Clone, Default)]
pub struct PostcardCodec;

/// Former name of [`PostcardCodec`].
#[deprecated(
    since = "0.6.0",
    note = "renamed to PostcardCodec; the default wire format is now postcard inside a versioned envelope, not bare bincode"
)]
pub type BincodeCodec = PostcardCodec;

#[derive(Serialize, Deserialize)]
struct StoredEntry {
    status: u16,
    version: u8,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
    tags: Option<Vec<String>>,
}

impl CacheCodec for PostcardCodec {
    const CODEC_ID: u8 = envelope::CODEC_POSTCARD;

    fn encode(&self, entry: &CacheEntry) -> Result<Vec<u8>, CacheError> {
        let stored = StoredEntry {
            status: entry.status.as_u16(),
            version: version_to_u8(entry.version),
            headers: entry.headers.clone(),
            body: entry.body.to_vec(),
            tags: entry.tags.clone(),
        };

        postcard::to_allocvec(&stored).map_err(|err| CacheError::Backend(err.to_string()))
    }

    fn decode(&self, bytes: &[u8]) -> Result<CacheEntry, CacheError> {
        let stored: StoredEntry =
            postcard::from_bytes(bytes).map_err(|err| CacheError::Backend(err.to_string()))?;
        // Deliberately a struct literal rather than `CacheEntry::new`, which
        // hardcodes `tags: None`. Routing through it is what dropped tags in
        // 0.5.x.
        Ok(CacheEntry {
            status: StatusCode::from_u16(stored.status)
                .map_err(|err| CacheError::Backend(err.to_string()))?,
            version: version_from_u8(stored.version)?,
            headers: stored.headers,
            body: Bytes::from(stored.body),
            tags: stored.tags,
        })
    }
}

pub(crate) fn version_to_u8(version: Version) -> u8 {
    match version {
        Version::HTTP_09 => 0,
        Version::HTTP_10 => 1,
        Version::HTTP_11 => 2,
        Version::HTTP_2 => 3,
        Version::HTTP_3 => 4,
        _ => 2,
    }
}

pub(crate) fn version_from_u8(value: u8) -> Result<Version, CacheError> {
    match value {
        0 => Ok(Version::HTTP_09),
        1 => Ok(Version::HTTP_10),
        2 => Ok(Version::HTTP_11),
        3 => Ok(Version::HTTP_2),
        4 => Ok(Version::HTTP_3),
        _ => Err(CacheError::Backend("unknown HTTP version".into())),
    }
}
