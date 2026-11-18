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

#[tokio::test]
async fn overwriting_key_removes_old_and_cleared_tags() {
    let backend = InMemoryBackend::new(32);

    let initial = tower_http_cache::backend::CacheEntry::new(
        http::StatusCode::OK,
        http::Version::HTTP_11,
        Vec::new(),
        bytes::Bytes::from_static(b"v1"),
    )
    .with_tags(vec!["old-tag".to_string(), "shared".to_string()]);

    backend
        .set(
            "k1".to_string(),
            initial,
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();

    // Overwrite with a different tag set.
    let updated = tower_http_cache::backend::CacheEntry::new(
        http::StatusCode::OK,
        http::Version::HTTP_11,
        Vec::new(),
        bytes::Bytes::from_static(b"v2"),
    )
    .with_tags(vec!["new-tag".to_string()]);

    backend
        .set(
            "k1".to_string(),
            updated,
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();

    assert!(backend.get_keys_by_tag("old-tag").await.unwrap().is_empty());
    assert!(backend.get_keys_by_tag("shared").await.unwrap().is_empty());
    assert_eq!(
        backend.get_keys_by_tag("new-tag").await.unwrap(),
        vec!["k1"]
    );
    assert_eq!(backend.list_tags().await.unwrap(), vec!["new-tag"]);

    // Overwrite with no tags at all.
    let untagged = tower_http_cache::backend::CacheEntry::new(
        http::StatusCode::OK,
        http::Version::HTTP_11,
        Vec::new(),
        bytes::Bytes::from_static(b"v3"),
    );
    backend
        .set(
            "k1".to_string(),
            untagged,
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();

    assert!(backend.get_keys_by_tag("new-tag").await.unwrap().is_empty());
    assert!(backend.list_tags().await.unwrap().is_empty());
}

#[tokio::test]
async fn expired_entries_are_pruned_from_tag_queries() {
    let backend = InMemoryBackend::new(32);
    let entry = tower_http_cache::backend::CacheEntry::new(
        http::StatusCode::OK,
        http::Version::HTTP_11,
        Vec::new(),
        bytes::Bytes::from_static(b"ephemeral"),
    )
    .with_tags(vec!["temp".to_string()]);

    backend
        .set(
            "k-exp".to_string(),
            entry,
            Duration::from_millis(15),
            Duration::ZERO,
        )
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(35)).await;

    assert!(backend.get_keys_by_tag("temp").await.unwrap().is_empty());
    assert!(backend.list_tags().await.unwrap().is_empty());
}
