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
}
