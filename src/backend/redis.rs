use std::time::Duration;

use async_trait::async_trait;
use redis::AsyncCommands;
use redis::aio::ConnectionManager;

use super::{CacheBackend, CacheEntry, CacheRead};
use crate::codec::envelope::{self, LegacyShape};
use crate::codec::{CacheCodec, PostcardCodec};
use crate::error::CacheError;

#[derive(Clone)]
pub struct RedisBackend<C = PostcardCodec> {
    // `ConnectionManager` is `pub struct ConnectionManager(Arc<Internals>)`
    // with `#[derive(Clone)]`, and it multiplexes internally. Wrapping it in
    // an `Arc<Mutex<..>>` -- as this field did up to 0.5.x -- serialised every
    // cache operation through one lock and defeated the multiplexing it was
    // wrapping. Clone it per operation instead; that is what the type is for.
    connection: ConnectionManager,
    namespace: String,
    codec: C,
}

impl RedisBackend<PostcardCodec> {
    /// Builds a backend over an already-connected [`ConnectionManager`].
    ///
    /// # Set the response timeout deliberately
    ///
    /// `redis` 1.x changed [`ConnectionManagerConfig`]'s defaults from *no
    /// timeouts* to a **500 ms response timeout and a 1 s connection
    /// timeout**. `Client::get_connection_manager()` uses those defaults.
    ///
    /// For an HTTP response cache that is a live hazard rather than a
    /// nicety: the values held here are whole response bodies, and a large
    /// entry over a loaded or cross-AZ Redis can take longer than 500 ms.
    /// Every such `get` or `set` then fails with [`CacheError::Redis`]
    /// instead of succeeding slowly — a slow cache silently becomes a
    /// broken one.
    ///
    /// This constructor takes an already-built `ConnectionManager`, so the
    /// crate cannot choose for you. Choose explicitly:
    ///
    /// ```no_run
    /// use redis::aio::ConnectionManagerConfig;
    /// use tower_http_cache::backend::redis::RedisBackend;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = redis::Client::open("redis://127.0.0.1/")?;
    ///
    /// // Restores the 0.5.x behaviour: no response or connection timeout.
    /// let config = ConnectionManagerConfig::new()
    ///     .set_response_timeout(None)
    ///     .set_connection_timeout(None);
    ///
    /// let manager = client.get_connection_manager_with_config(config).await?;
    /// let backend = RedisBackend::new(manager);
    /// # let _ = backend;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// A generous bound (say, several seconds) is usually a better answer than
    /// `None` — but pick it against your body sizes, not against the client
    /// library's default.
    ///
    /// [`ConnectionManagerConfig`]: redis::aio::ConnectionManagerConfig
    /// [`CacheError::Redis`]: crate::error::CacheError::Redis
    pub fn new(connection: ConnectionManager) -> Self {
        Self {
            connection,
            namespace: "tower_http_cache".to_owned(),
            codec: PostcardCodec,
        }
    }
}

impl<C> RedisBackend<C> {
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    pub fn with_codec<NC>(self, codec: NC) -> RedisBackend<NC> {
        RedisBackend {
            connection: self.connection,
            namespace: self.namespace,
            codec,
        }
    }

    fn make_key(&self, key: &str) -> String {
        format!("{}:{}", self.namespace, key)
    }
}

#[async_trait]
impl<C> CacheBackend for RedisBackend<C>
where
    C: CacheCodec,
{
    async fn get(&self, key: &str) -> Result<Option<CacheRead>, CacheError> {
        let mut conn = self.connection.clone();
        let data: Option<Vec<u8>> = conn.get(self.make_key(key)).await?;

        match data {
            Some(bytes) => envelope::read_stored(&bytes, &self.codec, LegacyShape::RedisOuter),
            None => Ok(None),
        }
    }

    async fn set(
        &self,
        key: String,
        entry: CacheEntry,
        ttl: Duration,
        stale_for: Duration,
    ) -> Result<(), CacheError> {
        if ttl.is_zero() {
            return Ok(());
        }

        let payload = self.codec.encode(&entry)?;

        let now_ms = envelope::current_millis()?;
        let expires_at_ms = now_ms.saturating_add(envelope::duration_millis(ttl));
        let stale_until_ms = expires_at_ms.saturating_add(envelope::duration_millis(stale_for));

        let bytes = envelope::wrap(C::CODEC_ID, expires_at_ms, stale_until_ms, &payload);

        let total_ttl = ttl.saturating_add(stale_for);
        let ttl_secs = total_ttl.as_secs().max(1);

        let mut conn = self.connection.clone();
        let _: () = conn.set_ex(self.make_key(&key), bytes, ttl_secs).await?;
        Ok(())
    }

    async fn invalidate(&self, key: &str) -> Result<(), CacheError> {
        let mut conn = self.connection.clone();
        let _: () = conn.del(self.make_key(key)).await?;
        Ok(())
    }

    /// Always [`CacheError::Unsupported`]: this backend keeps no tag index.
    ///
    /// 0.6.0 puts tags on the wire, so a `CacheRead` from this backend carries
    /// the tags the entry was stored with — but there is no reverse index, so
    /// tag -> keys cannot be answered. Reporting it is deliberate: inheriting
    /// the trait default would answer `Ok(vec![])`, and the caller could not
    /// tell that from "nothing carried that tag". A Redis-native index is
    /// planned for 0.7.0; memcached has no set type and will keep reporting
    /// this.
    async fn get_keys_by_tag(&self, _tag: &str) -> Result<Vec<String>, CacheError> {
        Err(unsupported_tags())
    }

    /// Always [`CacheError::Unsupported`]: this backend keeps no tag index.
    ///
    /// 0.6.0 puts tags on the wire, so a `CacheRead` from this backend carries
    /// the tags the entry was stored with — but there is no reverse index, so
    /// tag -> keys cannot be answered. Reporting it is deliberate: inheriting
    /// the trait default would answer `Ok(vec![])`, and the caller could not
    /// tell that from "nothing carried that tag". A Redis-native index is
    /// planned for 0.7.0; memcached has no set type and will keep reporting
    /// this.
    async fn list_tags(&self) -> Result<Vec<String>, CacheError> {
        Err(unsupported_tags())
    }
}

fn unsupported_tags() -> CacheError {
    CacheError::Unsupported(
        "RedisBackend keeps no tag index; tag lookup and tag invalidation are \
         not available on it. Planned for 0.7.0 as an opt-in Redis-native index."
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync_clone_static<T: Send + Sync + Clone + 'static>() {}

    /// Removing the `Arc<Mutex<..>>` must not change what `RedisBackend` is:
    /// `CacheBackend` requires all four bounds, and `ConnectionManager`
    /// supplies them on its own because it is already `Arc`-backed and
    /// `Clone`.
    #[test]
    fn redis_backend_is_send_sync_clone_static() {
        assert_send_sync_clone_static::<RedisBackend<PostcardCodec>>();
    }
}
