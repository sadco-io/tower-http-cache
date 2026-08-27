//! Tag support through the middleware.
//!
//! Up to 0.5.x `CachePolicy::extract_tags` had no callers anywhere in the
//! crate: both places where the layer builds a `CacheEntry` called
//! `CacheEntry::new` and never attached tags, so `with_tag_extractor` — the
//! mechanism the README documents — silently did nothing on every backend.

use std::time::Duration;

use http::{Request, Response};
use http_body_util::{BodyExt, Full};
use tower::service_fn;
use tower::{Layer, Service, ServiceExt};
use tower_http_cache::backend::memory::InMemoryBackend;
use tower_http_cache::prelude::*;

/// A1b. Fails on 0.5.x: the tag index stays empty because the layer never
/// calls `extract_tags`.
#[tokio::test]
async fn tag_extractor_populates_the_backend_tag_index() {
    let backend = InMemoryBackend::new(32);

    let policy = CachePolicy::default()
        .with_tag_policy(TagPolicy::new().with_enabled(true))
        .with_tag_extractor(|_method: &http::Method, uri: &http::Uri| {
            vec![format!("path:{}", uri.path()), "tenant:acme".to_string()]
        });

    let layer = CacheLayer::builder(backend.clone())
        .ttl(Duration::from_secs(60))
        .policy(policy)
        .build();

    let mut service = layer.layer(service_fn(|_req: Request<()>| async move {
        Ok::<_, std::convert::Infallible>(Response::new(Full::from("payload")))
    }));

    let response = service
        .ready()
        .await
        .unwrap()
        .call(Request::builder().uri("/api/widgets").body(()).unwrap())
        .await
        .unwrap();
    let _ = response.into_body().collect().await.unwrap();

    let mut tags = backend.list_tags().await.unwrap();
    tags.sort();
    assert_eq!(
        tags,
        vec!["path:/api/widgets".to_string(), "tenant:acme".to_string()],
        "the layer must attach the extracted tags to the stored entry"
    );

    let keys = backend.get_keys_by_tag("tenant:acme").await.unwrap();
    assert_eq!(keys.len(), 1, "the tag must map to the stored key");

    let invalidated = backend.invalidate_by_tag("tenant:acme").await.unwrap();
    assert_eq!(invalidated, 1);
    assert!(backend.get(&keys[0]).await.unwrap().is_none());
}

/// W8a must be inert for anyone who has not opted in: `TagPolicy::enabled`
/// defaults to `false`, and `extract_tags` returns an empty vector when it is.
#[tokio::test]
async fn default_tag_policy_leaves_the_index_empty() {
    let backend = InMemoryBackend::new(32);

    let policy =
        CachePolicy::default().with_tag_extractor(|_method: &http::Method, _uri: &http::Uri| {
            vec!["tenant:acme".to_string()]
        });

    let layer = CacheLayer::builder(backend.clone())
        .ttl(Duration::from_secs(60))
        .policy(policy)
        .build();

    let mut service = layer.layer(service_fn(|_req: Request<()>| async move {
        Ok::<_, std::convert::Infallible>(Response::new(Full::from("payload")))
    }));
    let response = service
        .ready()
        .await
        .unwrap()
        .call(Request::builder().uri("/api/widgets").body(()).unwrap())
        .await
        .unwrap();
    let _ = response.into_body().collect().await.unwrap();

    assert!(
        backend.list_tags().await.unwrap().is_empty(),
        "tags must stay off unless TagPolicy::enabled is set"
    );
}
