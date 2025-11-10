use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::{CacheBackend, CacheEntry, CacheRead};
use crate::codec::{BincodeCodec, CacheCodec};
use crate::error::CacheError;

#[derive(Clone)]
pub struct RedisBackend<C = BincodeCodec> {
    connection: Arc<Mutex<ConnectionManager>>,
    namespace: String,
    codec: C,
}

impl RedisBackend<BincodeCodec> {
    pub fn new(connection: ConnectionManager) -> Self {
        Self {
            connection: Arc::new(Mutex::new(connection)),
            namespace: "tower_http_cache".to_owned(),
            codec: BincodeCodec,
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

#[derive(Serialize, Deserialize)]
struct RedisRecord {
    payload: Vec<u8>,
    expires_at_ms: u64,
    stale_until_ms: u64,
}

#[async_trait]
impl<C> CacheBackend for RedisBackend<C>
where
    C: CacheCodec,
{
    async fn get(&self, key: &str) -> Result<Option<CacheRead>, CacheError> {
        let mut conn = self.connection.lock().await;
        let data: Option<Vec<u8>> = conn.get(self.make_key(key)).await?;

        if let Some(bytes) = data {
            let record: RedisRecord =
                bincode::deserialize(&bytes).map_err(|err| CacheError::Backend(err.to_string()))?;
            let entry = self.codec.decode(&record.payload)?;
            Ok(Some(CacheRead {
                entry,
                expires_at: Some(unix_ms_to_system_time(record.expires_at_ms)?),
                stale_until: Some(unix_ms_to_system_time(record.stale_until_ms)?),
            }))
        } else {
            Ok(None)
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

        let now_ms = current_millis()?;
        let expires_at_ms = now_ms.saturating_add(duration_millis(ttl));
        let stale_until_ms = expires_at_ms.saturating_add(duration_millis(stale_for));

        let record = RedisRecord {
            payload,
            expires_at_ms,
            stale_until_ms,
        };
        let bytes =
            bincode::serialize(&record).map_err(|err| CacheError::Backend(err.to_string()))?;

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

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

fn current_millis() -> Result<u64, CacheError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| CacheError::Backend(err.to_string()))?
        .as_millis() as u64)
}

fn unix_ms_to_system_time(ms: u64) -> Result<SystemTime, CacheError> {
    Ok(UNIX_EPOCH + Duration::from_millis(ms))
}
