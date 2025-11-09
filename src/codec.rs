use bytes::Bytes;
use http::{StatusCode, Version};
use serde::{Deserialize, Serialize};

use crate::backend::CacheEntry;
use crate::error::CacheError;

/// Trait representing a serialization strategy for cached entries.
pub trait CacheCodec: Send + Sync + Clone + 'static {
    fn encode(&self, entry: &CacheEntry) -> Result<Vec<u8>, CacheError>;
    fn decode(&self, bytes: &[u8]) -> Result<CacheEntry, CacheError>;
}

/// Default [`CacheCodec`] implementation backed by `bincode`.
#[derive(Clone, Default)]
pub struct BincodeCodec;

#[derive(Serialize, Deserialize)]
struct StoredEntry {
    status: u16,
    version: u8,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
}

impl CacheCodec for BincodeCodec {
    fn encode(&self, entry: &CacheEntry) -> Result<Vec<u8>, CacheError> {
        let stored = StoredEntry {
            status: entry.status.as_u16(),
            version: version_to_u8(entry.version),
            headers: entry.headers.clone(),
            body: entry.body.to_vec(),
        };

        bincode::serialize(&stored).map_err(|err| CacheError::Backend(err.to_string()))
    }

    fn decode(&self, bytes: &[u8]) -> Result<CacheEntry, CacheError> {
        let stored: StoredEntry =
            bincode::deserialize(bytes).map_err(|err| CacheError::Backend(err.to_string()))?;
        let entry = CacheEntry::new(
            StatusCode::from_u16(stored.status)
                .map_err(|err| CacheError::Backend(err.to_string()))?,
            version_from_u8(stored.version)?,
            stored.headers,
            Bytes::from(stored.body),
        );
        Ok(entry)
    }
}

fn version_to_u8(version: Version) -> u8 {
    match version {
        Version::HTTP_09 => 0,
        Version::HTTP_10 => 1,
        Version::HTTP_11 => 2,
        Version::HTTP_2 => 3,
        Version::HTTP_3 => 4,
        _ => 2,
    }
}

fn version_from_u8(value: u8) -> Result<Version, CacheError> {
    match value {
        0 => Ok(Version::HTTP_09),
        1 => Ok(Version::HTTP_10),
        2 => Ok(Version::HTTP_11),
        3 => Ok(Version::HTTP_2),
        4 => Ok(Version::HTTP_3),
        _ => Err(CacheError::Backend("unknown HTTP version".into())),
    }
}
