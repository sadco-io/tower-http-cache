#![cfg(feature = "redis-backend")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use http::Request;
use http::Response;
use http_body_util::{BodyExt, Full};
use redis::Client;
use redis::aio::ConnectionManagerConfig;
use tower::service_fn;
use tower::{Service, ServiceBuilder, ServiceExt};
use tower_http_cache::prelude::*;

/// redis 1.x defaults to a 500 ms response timeout and a 1 s connection
/// timeout. A response cache holds whole response bodies, so those defaults can
/// convert a slow success into an error. The tests pick their own bound rather
/// than inheriting one meant for small values.
fn manager_config() -> ConnectionManagerConfig {
    ConnectionManagerConfig::new()
        .set_response_timeout(Some(Duration::from_secs(10)))
        .set_connection_timeout(Some(Duration::from_secs(5)))
}

fn unique_namespace(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

async fn cleanup_namespace(
    conn: &mut redis::aio::ConnectionManager,
    namespace: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let pattern = format!("{namespace}:*");
    let mut cursor: u64 = 0;
    loop {
        let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(&pattern)
            .arg("COUNT")
            .arg(100)
            .query_async(conn)
            .await?;
        if !keys.is_empty() {
            redis::cmd("DEL").arg(&keys).query_async::<()>(conn).await?;
        }
        cursor = next_cursor;
        if cursor == 0 {
            break;
        }
    }
    Ok(())
}

#[tokio::test]
async fn redis_backend_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let redis_url = match std::env::var("REDIS_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("skipping redis integration test: set REDIS_URL");
            return Ok(());
        }
    };

    let client = Client::open(redis_url.clone())?;
    let manager = client
        .get_connection_manager_with_config(manager_config())
        .await?;

    let mut conn = manager.clone();
    let namespace = unique_namespace("thc_round_trip");
    let backend = RedisBackend::new(manager).with_namespace(&namespace);

    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(1))
        .stale_while_revalidate(Duration::from_secs(2))
        .build();

    let counter = Arc::new(AtomicUsize::new(0));

    let mut svc = ServiceBuilder::new().layer(layer).service(service_fn({
        let counter = counter.clone();
        move |_req: Request<()>| {
            let value = counter.fetch_add(1, Ordering::SeqCst) + 1;
            let body = Full::from(value.to_string());
            async move { Ok::<_, std::convert::Infallible>(Response::new(body)) }
        }
    }));

    let first = svc
        .ready()
        .await
        .map_err(|e| format!("ready error: {}", e))?
        .call(Request::new(()))
        .await
        .map_err(|e| format!("call error: {}", e))?
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("collect error: {}", e))?
        .to_bytes();
    assert_eq!(first, "1");

    let second = svc
        .ready()
        .await
        .map_err(|e| format!("ready error: {}", e))?
        .call(Request::new(()))
        .await
        .map_err(|e| format!("call error: {}", e))?
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("collect error: {}", e))?
        .to_bytes();
    assert_eq!(second, "1");

    assert_eq!(counter.load(Ordering::SeqCst), 1);
    cleanup_namespace(&mut conn, &namespace).await?;
    Ok(())
}

/// W8b: the shared backends report that they have no tag index instead of
/// answering `Ok(vec![])` / `Ok(0)`, which a caller could not distinguish from
/// "nothing carried that tag".
#[tokio::test]
async fn redis_backend_reports_tags_unsupported() -> Result<(), Box<dyn std::error::Error>> {
    let redis_url = match std::env::var("REDIS_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("skipping redis integration test: set REDIS_URL");
            return Ok(());
        }
    };

    let client = Client::open(redis_url)?;
    let namespace = unique_namespace("thc_tag_test");
    let backend = RedisBackend::new(
        client
            .get_connection_manager_with_config(manager_config())
            .await?,
    )
    .with_namespace(&namespace);

    let err = backend.get_keys_by_tag("tenant:acme").await.unwrap_err();
    assert!(err.is_unsupported(), "get_keys_by_tag: {err}");

    let err = backend.list_tags().await.unwrap_err();
    assert!(err.is_unsupported(), "list_tags: {err}");

    // The trait's defaulted `invalidate_by_tag` iterates `get_keys_by_tag`, so
    // it propagates rather than reporting a silent zero.
    let err = backend.invalidate_by_tag("tenant:acme").await.unwrap_err();
    assert!(err.is_unsupported(), "invalidate_by_tag: {err}");

    Ok(())
}

/// Unrecognised bytes planted in Redis must read cleanly as a miss (`Ok(None)`),
/// and what `RedisBackend` writes must start with the envelope magic so
/// `redis-cli --no-raw GET` shows `THC`.
#[tokio::test]
async fn redis_ignores_unrecognised_bytes_and_writes_the_envelope()
-> Result<(), Box<dyn std::error::Error>> {
    let redis_url = match std::env::var("REDIS_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("skipping redis integration test: set REDIS_URL");
            return Ok(());
        }
    };

    let client = Client::open(redis_url)?;
    let manager = client
        .get_connection_manager_with_config(manager_config())
        .await?;
    let mut conn = manager.clone();

    let namespace = unique_namespace("thc_xver_test");
    let backend = RedisBackend::new(manager).with_namespace(&namespace);

    redis::cmd("SET")
        .arg(format!("{namespace}:foreign"))
        .arg(b"not-a-valid-thc-envelope".as_slice())
        .query_async::<()>(&mut conn)
        .await?;

    let read = backend.get("foreign").await?;
    assert!(read.is_none(), "unrecognised bytes must read as a miss");

    // And what 0.7.0 writes carries the envelope.
    let entry = CacheEntry::new(
        http::StatusCode::OK,
        http::Version::HTTP_11,
        vec![("content-type".to_string(), b"text/plain".to_vec())],
        bytes::Bytes::from_static(b"fresh"),
    );
    backend
        .set(
            "fresh".to_string(),
            entry,
            Duration::from_secs(60),
            Duration::from_secs(60),
        )
        .await?;

    let raw: Vec<u8> = redis::cmd("GET")
        .arg(format!("{namespace}:fresh"))
        .query_async(&mut conn)
        .await?;
    assert_eq!(&raw[0..3], b"THC");
    assert_eq!(raw[3], tower_http_cache::codec::envelope::FORMAT_V2);

    let read = backend.get("fresh").await?.expect("hit");
    assert_eq!(read.entry.body, bytes::Bytes::from_static(b"fresh"));

    // Plant a 0.6.x FORMAT_V1 entry (5-field postcard layout without `compressed`)
    // and verify 0.7.0 reads it seamlessly with `compressed == false`.
    #[derive(serde::Serialize)]
    struct StoredEntryV06<'a> {
        status: u16,
        version: u8,
        headers: Vec<(&'a str, &'a [u8])>,
        body: &'a [u8],
        tags: Option<Vec<&'a str>>,
    }
    let v06_payload = postcard::to_allocvec(&StoredEntryV06 {
        status: 200,
        version: 2,
        headers: vec![("content-type", b"text/plain")],
        body: b"from-0.6.x",
        tags: Some(vec!["legacy:tag"]),
    })?;
    let future_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64
        + 60_000;
    let mut v1_bytes = Vec::with_capacity(
        tower_http_cache::codec::envelope::ENVELOPE_HEADER_LEN + v06_payload.len(),
    );
    v1_bytes.extend_from_slice(&tower_http_cache::codec::envelope::MAGIC);
    v1_bytes.push(tower_http_cache::codec::envelope::FORMAT_V1);
    v1_bytes.push(tower_http_cache::codec::envelope::CODEC_POSTCARD);
    v1_bytes.extend_from_slice(&future_ms.to_le_bytes());
    v1_bytes.extend_from_slice(&(future_ms + 60_000).to_le_bytes());
    v1_bytes.extend_from_slice(&v06_payload);

    redis::cmd("SET")
        .arg(format!("{namespace}:v06-entry"))
        .arg(v1_bytes.as_slice())
        .query_async::<()>(&mut conn)
        .await?;

    let v06_read = backend
        .get("v06-entry")
        .await?
        .expect("0.6.x FORMAT_V1 entry must read seamlessly from Redis");
    assert_eq!(
        v06_read.entry.body,
        bytes::Bytes::from_static(b"from-0.6.x")
    );
    assert_eq!(v06_read.entry.tags, Some(vec!["legacy:tag".to_string()]));
    assert!(!v06_read.entry.compressed);

    cleanup_namespace(&mut conn, &namespace).await?;
    Ok(())
}

#[cfg(feature = "in-memory")]
#[tokio::test]
async fn multi_tier_tag_invalidation_clears_redis_l2() -> Result<(), Box<dyn std::error::Error>> {
    let redis_url = match std::env::var("REDIS_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("skipping redis integration test: set REDIS_URL");
            return Ok(());
        }
    };

    let client = Client::open(redis_url)?;
    let manager = client
        .get_connection_manager_with_config(manager_config())
        .await?;
    let mut conn = manager.clone();
    let namespace = unique_namespace("thc_mt_tag_test");

    let l1 = InMemoryBackend::new(100);
    let l2 = RedisBackend::new(manager).with_namespace(&namespace);
    let mt = MultiTierBackend::new(l1.clone(), l2.clone());

    let entry = CacheEntry::new(
        http::StatusCode::OK,
        http::Version::HTTP_11,
        vec![],
        bytes::Bytes::from_static(b"tagged"),
    )
    .with_tags(vec!["tenant:42".to_string()]);
    mt.set(
        "k1".to_string(),
        entry,
        Duration::from_secs(60),
        Duration::ZERO,
    )
    .await?;

    let invalidated = mt.invalidate_by_tag("tenant:42").await?;
    assert_eq!(invalidated, 1);
    assert!(mt.get("k1").await?.is_none());
    assert!(l1.get("k1").await?.is_none());
    assert!(
        l2.get("k1").await?.is_none(),
        "L2 Redis copy must also be removed"
    );

    cleanup_namespace(&mut conn, &namespace).await?;
    Ok(())
}
