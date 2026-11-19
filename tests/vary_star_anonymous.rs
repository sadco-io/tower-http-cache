//! Regression tests for `Vary: *` handling on anonymous requests (RFC 9111 §4.1).
use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderValue, Request, StatusCode, Version};
use http_body_util::{BodyExt, Full};
use tower::{Layer, Service, ServiceExt, service_fn};
use tower_http_cache::policy::CachePolicy;
use tower_http_cache::prelude::*;

#[tokio::test]
async fn vary_star_is_never_reused_for_anonymous_requests() {
    for vary in ["*", "Accept-Language, *"] {
        let calls = Arc::new(AtomicUsize::new(0));
        let backend = InMemoryBackend::new(16);
        let layer = CacheLayer::builder(backend.clone())
            .ttl(Duration::from_secs(60))
            .build();
        let mut svc = layer.layer(service_fn({
            let calls = calls.clone();
            move |_req: Request<()>| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    Ok::<_, Infallible>(
                        http::Response::builder()
                            .header("cache-control", "public, max-age=60")
                            .header("vary", vary)
                            .body(Full::from(format!("call #{n}")))
                            .unwrap(),
                    )
                }
            }
        }));
        for expected in ["call #1", "call #2"] {
            let body = svc
                .ready()
                .await
                .unwrap()
                .call(Request::get("/r").body(()).unwrap())
                .await
                .unwrap()
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes();
            assert_eq!(String::from_utf8_lossy(&body), expected, "Vary: {vary}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2, "Vary: {vary}");
        assert!(
            backend.get("/r").await.unwrap().is_none(),
            "Vary: {vary} response must not be stored"
        );
    }
}

#[tokio::test]
async fn pre_stored_vary_star_entry_is_not_served_to_anonymous_requests() {
    let calls = Arc::new(AtomicUsize::new(0));
    let backend = InMemoryBackend::new(16);

    // Simulate a pre-existing backend entry written by an older node with `Vary: *`.
    backend
        .set(
            "/preexisting".to_string(),
            CacheEntry::new(
                StatusCode::OK,
                Version::HTTP_11,
                vec![
                    ("cache-control".to_string(), b"public, max-age=60".to_vec()),
                    ("vary".to_string(), b"*".to_vec()),
                ],
                Bytes::from_static(b"stale-entry"),
            ),
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();

    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(60))
        .build();
    let mut svc = layer.layer(service_fn({
        let calls = calls.clone();
        move |_req: Request<()>| {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                Ok::<_, Infallible>(
                    http::Response::builder()
                        .header("cache-control", "public, max-age=60")
                        .header("vary", "*")
                        .body(Full::from(format!("origin #{n}")))
                        .unwrap(),
                )
            }
        }
    }));

    let body = svc
        .ready()
        .await
        .unwrap()
        .call(Request::get("/preexisting").body(()).unwrap())
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(String::from_utf8_lossy(&body), "origin #1");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn named_vary_header_still_caches_for_anonymous_requests() {
    let calls = Arc::new(AtomicUsize::new(0));
    let layer = CacheLayer::builder(InMemoryBackend::new(16))
        .ttl(Duration::from_secs(60))
        .build();
    let mut svc = layer.layer(service_fn({
        let calls = calls.clone();
        move |_req: Request<()>| {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                Ok::<_, Infallible>(
                    http::Response::builder()
                        .header("cache-control", "public, max-age=60")
                        .header("vary", "Origin")
                        .body(Full::from(format!("call #{n}")))
                        .unwrap(),
                )
            }
        }
    }));

    for _ in 0..2 {
        let body = svc
            .ready()
            .await
            .unwrap()
            .call(Request::get("/named").body(()).unwrap())
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        assert_eq!(String::from_utf8_lossy(&body), "call #1");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

fn build_vary_response(n: usize, varies: &[HeaderValue]) -> http::Response<Full<Bytes>> {
    let mut response = http::Response::builder()
        .header("cache-control", "public, max-age=60")
        .header("content-type", "text/plain")
        .body(Full::from(format!("origin-body-0123456789 #{n}")))
        .unwrap();
    for v in varies {
        response.headers_mut().append(http::header::VARY, v.clone());
    }
    response
}

async fn collect_body<S>(svc: &mut S, req: Request<()>) -> String
where
    S: Service<Request<()>, Response = http::Response<tower_http_cache::layer::SyncBoxBody>>,
    S::Error: std::fmt::Debug,
{
    let response = svc.ready().await.unwrap().call(req).await.unwrap();
    String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}

fn chunk_policy() -> CachePolicy {
    CachePolicy::default()
        .with_ttl(Duration::from_secs(60))
        .with_streaming_policy(StreamingPolicy {
            enable_chunk_cache: true,
            chunk_size: 4,
            min_chunk_file_size: 4,
            excluded_content_types: HashSet::new(),
            range_handling: RangeHandling::CacheFullServeRanges,
            ..Default::default()
        })
}

#[tokio::test]
async fn multiple_vary_field_lines_with_star_are_not_reused() {
    let cases: Vec<Vec<HeaderValue>> = vec![
        vec![
            HeaderValue::from_static("Accept-Language"),
            HeaderValue::from_static("*"),
        ],
        vec![
            HeaderValue::from_static("*"),
            HeaderValue::from_static("Accept-Language"),
        ],
        vec![
            HeaderValue::from_static("Origin"),
            HeaderValue::from_static("accept-language, *"),
        ],
    ];
    for varies in cases {
        let calls = Arc::new(AtomicUsize::new(0));
        let backend = InMemoryBackend::new(16);
        let layer = CacheLayer::builder(backend.clone())
            .ttl(Duration::from_secs(60))
            .build();
        let varies_for_svc = varies.clone();
        let mut svc = layer.layer(service_fn({
            let calls = calls.clone();
            move |_req: Request<()>| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                let v = varies_for_svc.clone();
                async move { Ok::<_, Infallible>(build_vary_response(n, &v)) }
            }
        }));
        for _ in 0..2 {
            collect_body(&mut svc, Request::get("/m").body(()).unwrap()).await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2, "{varies:?}");
        assert!(backend.get("/m").await.unwrap().is_none(), "{varies:?}");
    }
}

#[tokio::test]
async fn pre_stored_mixed_case_and_split_vary_star_is_not_served() {
    for headers in [
        vec![("VARY".to_string(), b"*".to_vec())],
        vec![
            ("Vary".to_string(), b"Origin".to_vec()),
            ("vary".to_string(), b"accept-language , *".to_vec()),
        ],
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let backend = InMemoryBackend::new(16);
        let mut stored_headers = headers.clone();
        stored_headers.push(("cache-control".to_string(), b"public, max-age=60".to_vec()));
        backend
            .set(
                "/p".to_string(),
                CacheEntry::new(
                    StatusCode::OK,
                    Version::HTTP_11,
                    stored_headers,
                    Bytes::from_static(b"prestored"),
                ),
                Duration::from_secs(60),
                Duration::ZERO,
            )
            .await
            .unwrap();
        let layer = CacheLayer::builder(backend)
            .ttl(Duration::from_secs(60))
            .build();
        let mut svc = layer.layer(service_fn({
            let calls = calls.clone();
            move |_req: Request<()>| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    Ok::<_, Infallible>(build_vary_response(n, &[HeaderValue::from_static("*")]))
                }
            }
        }));
        let body = collect_body(&mut svc, Request::get("/p").body(()).unwrap()).await;
        assert_ne!(body, "prestored", "{headers:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn chunk_cache_populates_without_vary_star() {
    let layer = CacheLayer::builder(InMemoryBackend::new(16))
        .policy(chunk_policy())
        .build();
    let chunks = layer.chunk_cache().expect("chunk cache enabled").clone();
    let mut svc = layer.layer(service_fn(move |_req: Request<()>| async move {
        Ok::<_, Infallible>(build_vary_response(1, &[]))
    }));
    collect_body(&mut svc, Request::get("/c").body(()).unwrap()).await;
    assert!(
        chunks.get("/c").is_some(),
        "control: chunk cache should populate without Vary: *"
    );
}

#[tokio::test]
async fn chunk_cache_skips_vary_star_and_refuses_pre_stored_vary_star_chunks() {
    // 1. Vary: * response never populates the chunk cache.
    let calls = Arc::new(AtomicUsize::new(0));
    let layer = CacheLayer::builder(InMemoryBackend::new(16))
        .policy(chunk_policy())
        .build();
    let chunks = layer.chunk_cache().expect("chunk cache enabled").clone();
    let mut svc =
        layer.layer(service_fn({
            let calls = calls.clone();
            move |_req: Request<()>| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    Ok::<_, Infallible>(build_vary_response(n, &[HeaderValue::from_static("*")]))
                }
            }
        }));
    collect_body(&mut svc, Request::get("/c").body(()).unwrap()).await;
    assert!(
        chunks.get("/c").is_none(),
        "Vary: * response must not populate the chunk cache"
    );

    // 2. A pre-stored chunk entry declaring Vary: * is not served to a Range request.
    chunks.replace_with_ttl(
        "/c".to_string(),
        ChunkMetadata {
            total_size: 8,
            content_type: "text/plain".into(),
            etag: None,
            last_modified: None,
            status: StatusCode::OK,
            version: Version::HTTP_11,
            headers: vec![("vary".into(), b"*".to_vec())],
        },
        Some(Duration::from_secs(60)),
        vec![Bytes::from_static(b"STAL"), Bytes::from_static(b"ECHK")],
    );
    let before = calls.load(Ordering::SeqCst);
    let body = collect_body(
        &mut svc,
        Request::get("/c")
            .header(http::header::RANGE, "bytes=0-3")
            .body(())
            .unwrap(),
    )
    .await;
    assert_ne!(body, "STAL", "pre-stored Vary: * chunk entry was served");
    assert_eq!(calls.load(Ordering::SeqCst), before + 1);
}

#[tokio::test]
async fn multi_tier_never_stores_vary_star_and_ignores_pre_stored_l2_entry() {
    let l1 = InMemoryBackend::new(16);
    let l2 = InMemoryBackend::new(16);
    let mt = MultiTierBackend::builder()
        .l1(l1.clone())
        .l2(l2.clone())
        .build();
    let calls = Arc::new(AtomicUsize::new(0));
    let layer = CacheLayer::builder(mt).ttl(Duration::from_secs(60)).build();
    let mut svc =
        layer.layer(service_fn({
            let calls = calls.clone();
            move |_req: Request<()>| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    Ok::<_, Infallible>(build_vary_response(n, &[HeaderValue::from_static("*")]))
                }
            }
        }));
    for _ in 0..2 {
        collect_body(&mut svc, Request::get("/t").body(()).unwrap()).await;
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(l1.get("/t").await.unwrap().is_none());
    assert!(l2.get("/t").await.unwrap().is_none());

    l2.set(
        "/t2".to_string(),
        CacheEntry::new(
            StatusCode::OK,
            Version::HTTP_11,
            vec![("vary".into(), b"*".to_vec())],
            Bytes::from_static(b"l2-prestored"),
        ),
        Duration::from_secs(60),
        Duration::ZERO,
    )
    .await
    .unwrap();
    let body = collect_body(&mut svc, Request::get("/t2").body(()).unwrap()).await;
    assert_ne!(body, "l2-prestored");
}
