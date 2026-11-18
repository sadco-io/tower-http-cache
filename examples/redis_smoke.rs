#![cfg_attr(not(feature = "redis-backend"), allow(dead_code))]

//! Minimal Redis-backed HTTP server for smoke testing tower-http-cache.
//!
//! Run with:
//! ```bash
//! REDIS_URL=redis://127.0.0.1:6379/ cargo run --example redis_smoke --features redis-backend
//! ```

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
use axum::error_handling::HandleErrorLayer;
#[cfg(feature = "redis-backend")]
use axum::http::StatusCode;
#[cfg(feature = "redis-backend")]
use axum::{Router, extract::State, response::Html, routing::get};
#[cfg(feature = "redis-backend")]
use redis::Client;
#[cfg(feature = "redis-backend")]
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
#[cfg(feature = "redis-backend")]
use tower::ServiceBuilder;
#[cfg(feature = "redis-backend")]
use tower_http_cache::layer::BoxError;
#[cfg(feature = "redis-backend")]
use tower_http_cache::prelude::*;

#[cfg(feature = "redis-backend")]
#[derive(Clone)]
struct AppState {
    counter: Arc<AtomicUsize>,
}

#[cfg(feature = "redis-backend")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/".into());
    let client = Client::open(redis_url)?;

    // redis 1.x defaults to a 500 ms response timeout and a 1 s connection
    // timeout. A response cache holds whole response bodies, so a large entry
    // over a loaded or cross-AZ Redis can exceed 500 ms -- and every such
    // operation would then fail instead of succeeding slowly. Choose the bound
    // against your own body sizes; this example lifts it rather than inheriting
    // a default meant for small values.
    let config = ConnectionManagerConfig::new()
        .set_response_timeout(Some(Duration::from_secs(10)))
        .set_connection_timeout(Some(Duration::from_secs(5)));
    let manager: ConnectionManager = client.get_connection_manager_with_config(config).await?;

    let backend = RedisBackend::new(manager);

    let cache_layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(5))
        .stale_while_revalidate(Duration::from_secs(10))
        .refresh_before(Duration::from_secs(2))
        .key_extractor(KeyExtractor::path())
        .build();

    let state = AppState {
        counter: Arc::new(AtomicUsize::new(0)),
    };

    let app = Router::new()
        .route("/", get(handler))
        .with_state(state)
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(|err: BoxError| async move {
                    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
                }))
                .layer(cache_layer),
        );

    let addr: SocketAddr = "127.0.0.1:3000".parse()?;
    println!("Redis smoke server listening at http://{addr}");
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}

#[cfg(feature = "redis-backend")]
async fn handler(State(state): State<AppState>) -> Html<String> {
    let value = state.counter.fetch_add(1, Ordering::SeqCst) + 1;
    Html(format!("Hello from backend call #{value}"))
}
