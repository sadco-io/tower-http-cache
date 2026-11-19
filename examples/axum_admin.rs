#![cfg_attr(
    not(all(feature = "in-memory", feature = "admin-api")),
    allow(dead_code)
)]

//! Complete Axum example wiring the authenticated admin API on a separate
//! internal-only listener.
//!
//! Run with:
//! `ADMIN_TOKEN=replace-with-strong-token cargo run --example axum_admin --features admin-api`
//!
//! Example requests:
//! - `curl http://127.0.0.1:3000/items/42`
//! - `curl http://127.0.0.1:3001/admin/cache/health`
//! - `curl -H "Authorization: Bearer $ADMIN_TOKEN" http://127.0.0.1:3001/admin/cache/stats`
//! - `curl -H "Authorization: Bearer $ADMIN_TOKEN" "http://127.0.0.1:3001/admin/cache/hot-keys?limit=10"`
//! - `curl -H "Authorization: Bearer $ADMIN_TOKEN" http://127.0.0.1:3001/admin/cache/tags`
//! - `curl -X POST -H "Authorization: Bearer $ADMIN_TOKEN" -H "Content-Type: application/json" -d '{"key":"/items/42"}' http://127.0.0.1:3001/admin/cache/invalidate`

#[cfg(not(all(feature = "in-memory", feature = "admin-api")))]
fn main() {
    eprintln!("Enable `in-memory` and `admin-api` features to run this example");
}

#[cfg(all(feature = "in-memory", feature = "admin-api"))]
use std::net::SocketAddr;
#[cfg(all(feature = "in-memory", feature = "admin-api"))]
use std::sync::Arc;
#[cfg(all(feature = "in-memory", feature = "admin-api"))]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(all(feature = "in-memory", feature = "admin-api"))]
use std::time::Duration;

#[cfg(all(feature = "in-memory", feature = "admin-api"))]
use axum::{
    Json, Router,
    error_handling::HandleErrorLayer,
    extract::{Path, Query, State},
    http::{
        HeaderMap, StatusCode,
        header::{AUTHORIZATION, WWW_AUTHENTICATE},
    },
    response::{IntoResponse, Response},
    routing::{get, post},
};
#[cfg(all(feature = "in-memory", feature = "admin-api"))]
use serde::Deserialize;
#[cfg(all(feature = "in-memory", feature = "admin-api"))]
use tower::ServiceBuilder;
#[cfg(all(feature = "in-memory", feature = "admin-api"))]
use tower_http_cache::admin::routes::{
    ErrorResponse, InvalidationRequest, handle_health, handle_hot_keys, handle_invalidate,
    handle_list_tags, handle_stats,
};
#[cfg(all(feature = "in-memory", feature = "admin-api"))]
use tower_http_cache::admin::{AdminConfig, AdminError, AdminState};
#[cfg(all(feature = "in-memory", feature = "admin-api"))]
use tower_http_cache::layer::BoxError;
#[cfg(all(feature = "in-memory", feature = "admin-api"))]
use tower_http_cache::prelude::*;

#[cfg(all(feature = "in-memory", feature = "admin-api"))]
#[derive(Deserialize)]
struct HotKeysQuery {
    limit: Option<usize>,
}

#[cfg(all(feature = "in-memory", feature = "admin-api"))]
fn auth_header(headers: &HeaderMap) -> Option<&str> {
    headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok())
}

#[cfg(all(feature = "in-memory", feature = "admin-api"))]
fn admin_error_response(err: AdminError) -> Response {
    match err {
        AdminError::Disabled => StatusCode::NOT_FOUND.into_response(),
        AdminError::Unauthorized => (
            StatusCode::UNAUTHORIZED,
            [(WWW_AUTHENTICATE, "Bearer")],
            Json(ErrorResponse::from(err)),
        )
            .into_response(),
        other => {
            let status =
                StatusCode::from_u16(other.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(ErrorResponse::from(other))).into_response()
        }
    }
}

#[cfg(all(feature = "in-memory", feature = "admin-api"))]
async fn admin_health() -> impl IntoResponse {
    Json(handle_health())
}

#[cfg(all(feature = "in-memory", feature = "admin-api"))]
async fn admin_stats(
    State(state): State<AdminState<InMemoryBackend>>,
    headers: HeaderMap,
) -> Response {
    match handle_stats(&state, auth_header(&headers)) {
        Ok(resp) => Json(resp).into_response(),
        Err(err) => admin_error_response(err),
    }
}

#[cfg(all(feature = "in-memory", feature = "admin-api"))]
async fn admin_hot_keys(
    State(state): State<AdminState<InMemoryBackend>>,
    headers: HeaderMap,
    Query(query): Query<HotKeysQuery>,
) -> Response {
    match handle_hot_keys(&state, auth_header(&headers), query.limit) {
        Ok(resp) => Json(resp).into_response(),
        Err(err) => admin_error_response(err),
    }
}

#[cfg(all(feature = "in-memory", feature = "admin-api"))]
async fn admin_list_tags(
    State(state): State<AdminState<InMemoryBackend>>,
    headers: HeaderMap,
) -> Response {
    match handle_list_tags(&state, auth_header(&headers)).await {
        Ok(resp) => Json(resp).into_response(),
        Err(err) => admin_error_response(err),
    }
}

#[cfg(all(feature = "in-memory", feature = "admin-api"))]
async fn admin_invalidate(
    State(state): State<AdminState<InMemoryBackend>>,
    headers: HeaderMap,
    Json(payload): Json<InvalidationRequest>,
) -> Response {
    match handle_invalidate(&state, auth_header(&headers), payload).await {
        Ok(resp) => Json(resp).into_response(),
        Err(err) => admin_error_response(err),
    }
}

#[cfg(all(feature = "in-memory", feature = "admin-api"))]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let token = match std::env::var("ADMIN_TOKEN") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!("error: ADMIN_TOKEN environment variable must be set and non-blank");
            std::process::exit(1);
        }
    };

    let backend = InMemoryBackend::new(10_000);
    let policy = CachePolicy::default()
        .with_ttl(Duration::from_secs(60))
        .with_tag_policy(TagPolicy::new().with_enabled(true))
        .with_tag_extractor(|_method, uri| {
            let mut tags = vec!["items".to_string()];
            if let Some(id) = uri.path().strip_prefix("/items/") {
                tags.push(format!("item:{id}"));
            }
            tags
        });
    let cache_layer = CacheLayer::builder(backend).policy(policy).build();

    let admin_config = AdminConfig::new()
        .with_enabled(true)
        .with_require_auth(true)
        .with_auth_token(token);
    let admin_state = AdminState::from_layer(&cache_layer, admin_config);

    let counter = Arc::new(AtomicUsize::new(0));
    let public_app = Router::new()
        .route(
            "/items/{id}",
            get({
                let counter = counter.clone();
                move |Path(id): Path<String>| {
                    let counter = counter.clone();
                    async move {
                        let call = counter.fetch_add(1, Ordering::SeqCst) + 1;
                        format!("item {id} (origin call #{call})")
                    }
                }
            }),
        )
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(|err: BoxError| async move {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Cache middleware error: {err}"),
                    )
                }))
                .layer(cache_layer),
        );

    // Mount admin routes on a dedicated internal-only listener.
    // If `AdminConfig::enabled` is `false`, do not mount any admin routes
    // (including `/admin/cache/health`) at all.
    let admin_router: Router = Router::new()
        .route("/admin/cache/health", get(admin_health))
        .route("/admin/cache/stats", get(admin_stats))
        .route("/admin/cache/hot-keys", get(admin_hot_keys))
        .route("/admin/cache/tags", get(admin_list_tags))
        .route("/admin/cache/invalidate", post(admin_invalidate))
        .with_state(admin_state);

    let public_addr: SocketAddr = "127.0.0.1:3000".parse()?;
    let admin_addr: SocketAddr = "127.0.0.1:3001".parse()?;
    let public_listener = tokio::net::TcpListener::bind(public_addr).await?;
    let admin_listener = tokio::net::TcpListener::bind(admin_addr).await?;

    println!("Public app listening on http://{public_addr}");
    println!("Internal admin API listening on http://{admin_addr}/admin/cache/*");

    tokio::try_join!(
        axum::serve(public_listener, public_app),
        axum::serve(admin_listener, admin_router),
    )?;
    Ok(())
}
