use thiserror::Error;

/// Errors that can occur while interacting with a cache backend.
///
/// Marked `#[non_exhaustive]` as of 0.6.0: match arms must include a `_`
/// fallback, and future variants are then additive rather than breaking.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CacheError {
    #[error("backend error: {0}")]
    Backend(String),

    /// The backend does not implement the requested operation.
    ///
    /// Returned by the shared backends for the tag-index operations
    /// (`get_keys_by_tag`, `list_tags`, and therefore `invalidate_by_tag`),
    /// which they cannot serve. Reporting it is deliberate: up to 0.5.x they
    /// inherited the trait defaults and answered `Ok(vec![])` / `Ok(0)`, so a
    /// caller could not tell "nothing carried that tag" from "this backend
    /// cannot do tags at all".
    #[error("unsupported by this backend: {0}")]
    Unsupported(String),

    #[cfg(feature = "redis-backend")]
    #[error(transparent)]
    Redis(#[from] redis::RedisError),
}

impl CacheError {
    /// Reports whether this is [`CacheError::Unsupported`].
    pub fn is_unsupported(&self) -> bool {
        matches!(self, CacheError::Unsupported(_))
    }
}
