#![cfg_attr(not(feature = "redis-backend"), allow(dead_code))]

//! Run with:
//! REDIS_URL=redis://127.0.0.1:6379/ cargo run --example axum_redis --features "redis-backend compression metrics tracing"

#[cfg(not(feature = "redis-backend"))]
fn main() {
    eprintln!("Enable `redis-backend` feature to run this example");
}

#[cfg(feature = "redis-backend")]
use std::net::SocketAddr;
#[cfg(feature = "redis-backend")]
use std::sync::Arc;
#[cfg(feature = "redis-backend")]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(feature = "redis-backend")]
use std::time::Duration;

#[cfg(feature = "redis-backend")]
use axum::{Router, routing::get};
#[cfg(feature = "redis-backend")]
use redis::Client;
#[cfg(feature = "redis-backend")]
use redis::aio::ConnectionManager;
#[cfg(feature = "redis-backend")]
use tower::ServiceBuilder;
#[cfg(feature = "redis-backend")]
use tower_http_cache::prelude::*;
#[cfg(feature = "redis-backend")]
use tracing_subscriber;

#[cfg(feature = "redis-backend")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/".into());
    let client = Client::open(redis_url)?;
    let manager: ConnectionManager = client.get_tokio_connection_manager().await?;

    let backend = RedisBackend::new(manager);

    let cache_layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(5))
        .stale_while_revalidate(Duration::from_secs(10))
        .refresh_before(Duration::from_secs(2))
        .max_body_size(Some(512 * 1024))
        .compression(CompressionConfig {
            strategy: CompressionStrategy::Gzip,
            min_size: 8 * 1024,
        })
        .key_extractor(KeyExtractor::path())
        .build();

    let counter = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/",
            get({
                let counter = counter.clone();
                move || {
                    let counter = counter.clone();
                    async move {
                        let value = counter.fetch_add(1, Ordering::SeqCst) + 1;
                        format!("Hello from backend call #{value}")
                    }
                }
            }),
        )
        .layer(ServiceBuilder::new().layer(cache_layer));

    let addr: SocketAddr = "127.0.0.1:3000".parse()?;
    println!("Listening on http://{addr}");
    println!("Try curling the endpoint multiple times to observe cached responses.");

    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}
