//! Reader for cache entries written by tower-http-cache 0.5.x.
//!
//! **Deprecated: this module and the `legacy-bincode1-read` feature that gates
//! it are removed in 0.7.0.** Cache entries are self-expiring, so once every
//! 0.5.x-written entry has aged past its TTL plus its stale window the feature
//! can be turned off. Disabling it is safe at any time; the only cost is a cold
//! cache.
//!
//! # Why this is hand-written
//!
//! Calling `bincode` here would keep `bincode 1.3.3` in the dependency graph
//! and keep the permanently-ignored RUSTSEC-2025-0141 suppression in
//! `deny.toml`, which is the thing 0.6.0 exists to clear. The bincode 1 default
//! configuration is a small, fixed encoding, so the three struct shapes 0.5.x
//! wrote are decoded directly:
//!
//! * fixed-width integers, little-endian;
//! * every sequence, string and byte array prefixed with its length as a
//!   little-endian `u64`;
//! * `Option` as a single `u8` tag, `0` for `None` and `1` for `Some`.
//!
//! # The two shapes are not symmetric
//!
//! ```text
//! Redis     bincode1(RedisRecord     { payload: bincode1(StoredEntry), expires_at_ms, stale_until_ms })
//! Memcached bincode1(MemcachedRecord { entry: CacheEntry,              expires_at_ms, stale_until_ms })
//! ```
//!
//! ## `version` is encoded differently in the two shapes
//!
//! 0.5.x's Redis payload holds `version` as an explicit `u8`. The memcached
//! shape serializes `CacheEntry` whole, and `CacheEntry`'s `version_serde`
//! helper wrote `let v = match .. { .. => 2, .. }` with no type annotation --
//! so the literals defaulted to `i32` and the field went out as **four bytes**
//! while the matching `deserialize` read one. Reading it as a `u8` here
//! desynchronises the cursor and fails on every real entry, so this reader
//! takes four bytes for the memcached shape and one for the Redis shape.
//!
//! (That same asymmetry meant 0.5.x's memcached backend could not read back
//! what it had itself written -- every memcached `get` failed to deserialize
//! and was served as a miss. 0.6.0 fixes the helper and routes both backends
//! through the codec, so the situation does not recur.)
//!
//! 0.5.x's Redis payload is a private `StoredEntry` with **no `tags` field**,
//! so [`decode_legacy_redis`] sets `tags: None` unconditionally. 0.5.x's
//! memcached record serialized `CacheEntry` whole, so [`decode_legacy_memcached`]
//! **does** read tags. Swapping those produces a decoder that fails on every
//! real entry.
//!
//! # Safety against hostile input
//!
//! Every length read from the buffer is checked against the bytes remaining
//! before it is used, so a corrupt `u64` length cannot drive a large
//! allocation, and no path can panic or read out of bounds. Both decoders also
//! require the buffer to be consumed exactly.

use bytes::Bytes;
use http::{StatusCode, Version};

use super::envelope::unix_ms_to_system_time;
use super::version_from_u8;
use crate::backend::{CacheEntry, CacheRead};
use crate::error::CacheError;

fn err(msg: impl Into<String>) -> CacheError {
    CacheError::Backend(format!("legacy bincode1 decode: {}", msg.into()))
}

/// Bounds-checked forward cursor over a byte slice.
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], CacheError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| err("length overflow"))?;
        if end > self.bytes.len() {
            return Err(err(format!(
                "unexpected end of input: need {} bytes, {} remain",
                n,
                self.remaining()
            )));
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, CacheError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, CacheError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32, CacheError> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, CacheError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    /// Reads a bincode-1 collection length: a little-endian `u64`, rejected
    /// immediately if it exceeds the bytes left in the buffer.
    ///
    /// This is the bound that keeps a corrupt length from reaching
    /// `Vec::with_capacity`.
    fn len(&mut self) -> Result<usize, CacheError> {
        let n = self.u64()?;
        if n > self.remaining() as u64 {
            return Err(err(format!(
                "declared length {} exceeds the {} bytes remaining",
                n,
                self.remaining()
            )));
        }
        Ok(n as usize)
    }

    fn byte_string(&mut self) -> Result<Vec<u8>, CacheError> {
        let n = self.len()?;
        Ok(self.take(n)?.to_vec())
    }

    fn string(&mut self) -> Result<String, CacheError> {
        let n = self.len()?;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|e| err(e.to_string()))
    }

    fn headers(&mut self) -> Result<Vec<(String, Vec<u8>)>, CacheError> {
        // A header pair costs at least 16 bytes (two u64 length prefixes), so
        // the count is bounded by the remaining buffer before any allocation.
        let n = self.len()?;
        let mut out = Vec::with_capacity(n.min(self.remaining() / 16 + 1));
        for _ in 0..n {
            let name = self.string()?;
            let value = self.byte_string()?;
            out.push((name, value));
        }
        Ok(out)
    }

    fn optional_tags(&mut self) -> Result<Option<Vec<String>>, CacheError> {
        match self.u8()? {
            0 => Ok(None),
            1 => {
                let n = self.len()?;
                let mut out = Vec::with_capacity(n.min(self.remaining() / 8 + 1));
                for _ in 0..n {
                    out.push(self.string()?);
                }
                Ok(Some(out))
            }
            tag => Err(err(format!("invalid Option tag {tag}"))),
        }
    }

    /// Rejects trailing bytes. Both 0.5.x records framed their value exactly,
    /// so leftovers mean this is not the shape we think it is.
    fn finish(self) -> Result<(), CacheError> {
        if self.pos != self.bytes.len() {
            return Err(err(format!(
                "{} trailing bytes after the record",
                self.remaining()
            )));
        }
        Ok(())
    }
}

/// Maps the version discriminant 0.5.x wrote into a [`Version`].
///
/// 0.5.x's `CacheEntry` deserializer mapped anything it did not recognise to
/// HTTP/1.1 rather than failing, so that fallback is reproduced here.
fn legacy_entry_version(value: i32) -> Version {
    match value {
        0 => Version::HTTP_09,
        1 => Version::HTTP_10,
        2 => Version::HTTP_11,
        3 => Version::HTTP_2,
        4 => Version::HTTP_3,
        _ => Version::HTTP_11,
    }
}

fn status_from_u16(value: u16) -> Result<StatusCode, CacheError> {
    StatusCode::from_u16(value).map_err(|e| err(e.to_string()))
}

/// Decodes a 0.5.x Redis value: `bincode1(RedisRecord)`.
///
/// The inner payload is 0.5.x's private `StoredEntry`, which carried no tags,
/// so the returned entry always has `tags: None`.
pub fn decode_legacy_redis(bytes: &[u8]) -> Result<CacheRead, CacheError> {
    let mut cursor = Cursor::new(bytes);
    let payload = cursor.byte_string()?;
    let expires_at_ms = cursor.u64()?;
    let stale_until_ms = cursor.u64()?;
    cursor.finish()?;

    let mut inner = Cursor::new(&payload);
    let status = inner.u16()?;
    let version = inner.u8()?;
    let headers = inner.headers()?;
    let body = inner.byte_string()?;
    inner.finish()?;

    Ok(CacheRead {
        entry: CacheEntry {
            status: status_from_u16(status)?,
            // 0.5.x's `StoredEntry` decoder rejected unknown version bytes.
            version: version_from_u8(version)?,
            headers,
            body: Bytes::from(body),
            tags: None,
        },
        expires_at: Some(unix_ms_to_system_time(expires_at_ms)),
        stale_until: Some(unix_ms_to_system_time(stale_until_ms)),
    })
}

/// Decodes a 0.5.x memcached value: `bincode1(MemcachedRecord)`.
///
/// The entry was serialized whole, so tags are present on the wire and are
/// preserved here.
pub fn decode_legacy_memcached(bytes: &[u8]) -> Result<CacheRead, CacheError> {
    let mut cursor = Cursor::new(bytes);
    let status = cursor.u16()?;
    // Four bytes, not one: see the module note on `version_serde`.
    let version = cursor.i32()?;
    let headers = cursor.headers()?;
    let body = cursor.byte_string()?;
    let tags = cursor.optional_tags()?;
    let expires_at_ms = cursor.u64()?;
    let stale_until_ms = cursor.u64()?;
    cursor.finish()?;

    Ok(CacheRead {
        entry: CacheEntry {
            status: status_from_u16(status)?,
            version: legacy_entry_version(version),
            headers,
            body: Bytes::from(body),
            tags,
        },
        expires_at: Some(unix_ms_to_system_time(expires_at_ms)),
        stale_until: Some(unix_ms_to_system_time(stale_until_ms)),
    })
}
