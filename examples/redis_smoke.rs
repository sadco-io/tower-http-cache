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
use axum::http::StatusCode;
#[cfg(feature = "redis-backend")]
use axum::{
    Router,
    extract::State,
    response::{Html, IntoResponse, Response},
    routing::get,
};
#[cfg(feature = "redis-backend")]
use http_body_util::{BodyExt, Full};
#[cfg(feature = "redis-backend")]
use redis::Client;
#[cfg(feature = "redis-backend")]
use redis::aio::ConnectionManager;
#[cfg(feature = "redis-backend")]
use tower::Layer;
#[cfg(feature = "redis-backend")]
use tower::ServiceExt;
#[cfg(feature = "redis-backend")]
use tower::service_fn;
#[cfg(feature = "redis-backend")]
use tower_http_cache::prelude::*;

#[cfg(feature = "redis-backend")]
#[derive(Clone)]
struct AppState {
    cache_layer: CacheLayer<RedisBackend>,
    counter: Arc<AtomicUsize>,
}

#[cfg(feature = "redis-backend")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/".into());
    let client = Client::open(redis_url)?;
    let manager: ConnectionManager = client.get_tokio_connection_manager().await?;

    let backend = RedisBackend::new(manager);

    let cache_layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(5))
        .stale_while_revalidate(Duration::from_secs(10))
        .refresh_before(Duration::from_secs(2))
        .key_extractor(KeyExtractor::path())
        .build();

    let state = AppState {
        cache_layer,
        counter: Arc::new(AtomicUsize::new(0)),
    };

    let app = Router::new().route("/", get(handler)).with_state(state);

    let addr: SocketAddr = "127.0.0.1:3000".parse()?;
    println!("Redis smoke server listening at http://{addr}");
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}

#[cfg(feature = "redis-backend")]
async fn handler(
    State(state): State<AppState>,
    req: axum::http::Request<axum::body::Body>,
) -> Result<Response, (StatusCode, String)> {
    let inner = service_fn({
        let counter = state.counter.clone();
        move |_req: http::Request<()>| {
            let counter = counter.clone();
            async move {
                let value = counter.fetch_add(1, Ordering::SeqCst) + 1;
                let body = format!("Hello from backend call #{value}");
                Ok::<_, std::convert::Infallible>(http::Response::new(Full::from(body)))
            }
        }
    });

    let response = state
        .cache_layer
        .clone()
        .layer(inner)
        .oneshot(req.map(|_| ()))
        .await
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;

    let collected = response.into_body().collect().await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to read body".into(),
        )
    })?;

    let body = collected.to_bytes();
    let body = String::from_utf8(body.to_vec()).unwrap_or_else(|_| "<invalid utf-8>".into());
    Ok(Html(body).into_response())
}
