use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use tokio::sync::Mutex;

use super::{CacheBackend, CacheEntry, CacheRead};
use crate::codec::envelope::{self, LegacyShape};
use crate::codec::{CacheCodec, PostcardCodec};
use crate::error::CacheError;

#[derive(Clone)]
pub struct RedisBackend<C = PostcardCodec> {
    connection: Arc<Mutex<ConnectionManager>>,
    namespace: String,
    codec: C,
}

impl RedisBackend<PostcardCodec> {
    pub fn new(connection: ConnectionManager) -> Self {
        Self {
            connection: Arc::new(Mutex::new(connection)),
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
        let mut conn = self.connection.lock().await;
        let data: Option<Vec<u8>> = conn.get(self.make_key(key)).await?;
        drop(conn);

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

        let mut conn = self.connection.lock().await;
        let _: () = conn.set_ex(self.make_key(&key), bytes, ttl_secs).await?;
        Ok(())
    }

    async fn invalidate(&self, key: &str) -> Result<(), CacheError> {
        let mut conn = self.connection.lock().await;
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
