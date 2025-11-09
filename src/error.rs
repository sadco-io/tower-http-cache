use thiserror::Error;

/// Errors that can occur while interacting with a cache backend.
#[derive(Debug, Error)]
pub enum CacheError {
    #[error("backend error: {0}")]
    Backend(String),

    #[cfg(feature = "redis-backend")]
    #[error(transparent)]
    Redis(#[from] redis::RedisError),
}
