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

    // Clean slate for the test DB
    let mut conn = manager.clone();
    redis::cmd("FLUSHDB").query_async::<()>(&mut conn).await?;

    let backend = RedisBackend::new(manager).with_namespace("tower_http_cache_test");

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
    let backend = RedisBackend::new(
        client
            .get_connection_manager_with_config(manager_config())
            .await?,
    )
    .with_namespace("thc_tag_test");

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

/// The end-to-end half of the cross-version claim: a value written by 0.5.1,
/// planted in Redis verbatim, must be a hit for 0.6.0 through the real
/// `GET` -> `Option<Vec<u8>>` path. The byte-level assertions live in
/// `tests/wire_compat.rs`; this one exercises the transport.
///
/// The reverse direction is asserted too: what 0.6.0 writes must start with
/// the envelope magic, so `redis-cli --no-raw GET` shows `THC`.
#[tokio::test]
async fn redis_reads_a_real_0_5_1_entry_and_writes_the_envelope()
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
    redis::cmd("FLUSHDB").query_async::<()>(&mut conn).await?;

    let namespace = "thc_xver_test";
    let backend = RedisBackend::new(manager).with_namespace(namespace);

    // A real 0.5.1 Redis value, byte for byte. See tests/fixtures/v0_5_1/.
    let fixture = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/v0_5_1/redis_s404_v2_h8_b512_ascii.bin"
    ))?;
    redis::cmd("SET")
        .arg(format!("{namespace}:legacy"))
        .arg(fixture.as_slice())
        .query_async::<()>(&mut conn)
        .await?;

    let read = backend
        .get("legacy")
        .await?
        .expect("a 0.5.1 entry must still be a hit under 0.6.0");
    assert_eq!(read.entry.status.as_u16(), 404);
    assert_eq!(read.entry.version, http::Version::HTTP_2);
    assert_eq!(read.entry.headers.len(), 8);
    assert_eq!(read.entry.body.len(), 512);
    assert_eq!(read.entry.tags, None, "0.5.x redis dropped tags");

    // And what 0.6.0 writes carries the envelope.
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

    let read = backend.get("fresh").await?.expect("hit");
    assert_eq!(read.entry.body, bytes::Bytes::from_static(b"fresh"));

    redis::cmd("FLUSHDB").query_async::<()>(&mut conn).await?;
    Ok(())
}
