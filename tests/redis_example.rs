#![cfg(feature = "redis-backend")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use http::Request;
use http::Response;
use http_body_util::{BodyExt, Full};
use redis::Client;
use tower::service_fn;
use tower::{Service, ServiceBuilder, ServiceExt};
use tower_http_cache::prelude::*;

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
    let manager = client.get_connection_manager().await?;

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
