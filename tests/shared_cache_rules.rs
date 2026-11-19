//! Regression tests for RFC 9111 shared-cache rules (`Authorization` and `Set-Cookie`).
use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http::header::CACHE_CONTROL;
use http::{Request, StatusCode, Version};
use http_body_util::{BodyExt, Full};
use tower::{Layer, Service, ServiceExt, service_fn};
use tower_http_cache::policy::CachePolicy;
use tower_http_cache::prelude::*;

#[tokio::test]
async fn authorization_responses_without_shared_cache_directive_are_not_shared() {
    // RFC 9111 §3.5: a response to an `Authorization` request without `public`,
    // `must-revalidate`, or `s-maxage` must not be stored or served to another
    // `Authorization` value or to an anonymous request.
    for cache_control in [None, Some("max-age=60")] {
        let calls = Arc::new(AtomicUsize::new(0));
        let backend = InMemoryBackend::new(64);
        let layer = CacheLayer::builder(backend.clone())
            .ttl(Duration::from_secs(60))
            .build();
        let mut svc = layer.layer(service_fn({
            let calls = calls.clone();
            move |req: Request<()>| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                let who = req
                    .headers()
                    .get(http::header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("anonymous")
                    .to_owned();
                async move {
                    let mut builder = http::Response::builder().status(StatusCode::OK);
                    if let Some(cc) = cache_control {
                        builder = builder.header(CACHE_CONTROL, cc);
                    }
                    Ok::<_, Infallible>(
                        builder
                            .body(Full::from(format!("account [{who}] #{n}")))
                            .unwrap(),
                    )
                }
            }
        }));

        let alice = svc
            .ready()
            .await
            .unwrap()
            .call(
                Request::get("/account")
                    .header(http::header::AUTHORIZATION, "Bearer alice-token")
                    .body(())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        assert_eq!(&alice[..], b"account [Bearer alice-token] #1");
        assert!(
            backend.get("/account").await.unwrap().is_none(),
            "Authorization response with Cache-Control={cache_control:?} must not be stored"
        );

        let bob = svc
            .ready()
            .await
            .unwrap()
            .call(
                Request::get("/account")
                    .header(http::header::AUTHORIZATION, "Bearer bob-token")
                    .body(())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        assert_eq!(&bob[..], b"account [Bearer bob-token] #2");

        let anon = svc
            .ready()
            .await
            .unwrap()
            .call(Request::get("/account").body(()).unwrap())
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        assert_eq!(&anon[..], b"account [anonymous] #3");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}

#[tokio::test]
async fn authorization_response_with_cache_control_public_may_be_shared() {
    let calls = Arc::new(AtomicUsize::new(0));
    let layer = CacheLayer::builder(InMemoryBackend::new(64))
        .ttl(Duration::from_secs(60))
        .build();
    let mut svc = layer.layer(service_fn({
        let calls = calls.clone();
        move |_req: Request<()>| {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                Ok::<_, Infallible>(
                    http::Response::builder()
                        .header(CACHE_CONTROL, "public, max-age=60")
                        .body(Full::from(format!("shared-public #{n}")))
                        .unwrap(),
                )
            }
        }
    }));

    let alice = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/public-catalog")
                .header(http::header::AUTHORIZATION, "Bearer alice-token")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let bob = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/public-catalog")
                .header(http::header::AUTHORIZATION, "Bearer bob-token")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let anon = svc
        .ready()
        .await
        .unwrap()
        .call(Request::get("/public-catalog").body(()).unwrap())
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();

    assert_eq!(&alice[..], b"shared-public #1");
    assert_eq!(&bob[..], b"shared-public #1");
    assert_eq!(&anon[..], b"shared-public #1");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn set_cookie_is_never_present_on_cached_responses() {
    // 1. Default policy, anonymous traffic: visitor 1 gets Set-Cookie from origin,
    //    stored entry omits Set-Cookie, and cached hit to visitor 2 omits Set-Cookie.
    for with_allowlist in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let backend = InMemoryBackend::new(64);
        let mut builder = CacheLayer::builder(backend.clone()).ttl(Duration::from_secs(60));
        if with_allowlist {
            builder = builder.header_allowlist(["content-type", "set-cookie"]);
        }
        let layer = builder.build();
        let mut svc = layer.layer(service_fn({
            let calls = calls.clone();
            move |_req: Request<()>| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    Ok::<_, Infallible>(
                        http::Response::builder()
                            .header("content-type", "text/plain")
                            .header(
                                http::header::SET_COOKIE,
                                format!("sid=session-{n}; Path=/; HttpOnly"),
                            )
                            .body(Full::from(format!("welcome #{n}")))
                            .unwrap(),
                    )
                }
            }
        }));

        let miss = svc
            .ready()
            .await
            .unwrap()
            .call(Request::get("/landing").body(()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            miss.headers().get(http::header::SET_COOKIE).unwrap(),
            "sid=session-1; Path=/; HttpOnly"
        );
        let miss_body = miss.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&miss_body[..], b"welcome #1");

        let stored = backend.get("/landing").await.unwrap().unwrap();
        assert!(
            !stored
                .entry
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("set-cookie")),
            "stored cache entry must never retain Set-Cookie (allowlist={with_allowlist})"
        );

        let hit = svc
            .ready()
            .await
            .unwrap()
            .call(Request::get("/landing").body(()).unwrap())
            .await
            .unwrap();
        assert!(
            hit.headers().get(http::header::SET_COOKIE).is_none(),
            "cached hit must never contain Set-Cookie (allowlist={with_allowlist})"
        );
        let hit_body = hit.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&hit_body[..], b"welcome #1");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    // 2. Authorization request whose response sets a session cookie (no public directive):
    //    not stored, and a subsequent anonymous request reaches origin instead of reusing it.
    let auth_calls = Arc::new(AtomicUsize::new(0));
    let auth_backend = InMemoryBackend::new(64);
    let auth_layer = CacheLayer::builder(auth_backend.clone())
        .ttl(Duration::from_secs(60))
        .build();
    let mut auth_svc = auth_layer.layer(service_fn({
        let calls = auth_calls.clone();
        move |_req: Request<()>| {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                Ok::<_, Infallible>(
                    http::Response::builder()
                        .header(
                            http::header::SET_COOKIE,
                            format!("sid=auth-{n}; Path=/; HttpOnly"),
                        )
                        .body(Full::from(format!("auth-session #{n}")))
                        .unwrap(),
                )
            }
        }
    }));

    let alice = auth_svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/session")
                .header(http::header::AUTHORIZATION, "Bearer alice-token")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        alice.headers().get(http::header::SET_COOKIE).unwrap(),
        "sid=auth-1; Path=/; HttpOnly"
    );
    assert!(auth_backend.get("/session").await.unwrap().is_none());

    let anon = auth_svc
        .ready()
        .await
        .unwrap()
        .call(Request::get("/session").body(()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        anon.headers().get(http::header::SET_COOKIE).unwrap(),
        "sid=auth-2; Path=/; HttpOnly"
    );
    let anon_body = anon.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&anon_body[..], b"auth-session #2");
    assert_eq!(auth_calls.load(Ordering::SeqCst), 2);

    // 3. Pre-stored entry (e.g. written by an earlier version in a persistent backend)
    //    carrying `Set-Cookie`: the cached hit strips `Set-Cookie` on read.
    let pre_calls = Arc::new(AtomicUsize::new(0));
    let pre_backend = InMemoryBackend::new(64);
    pre_backend
        .set(
            "/legacy".to_string(),
            CacheEntry::new(
                StatusCode::OK,
                Version::HTTP_11,
                vec![
                    ("content-type".to_string(), b"text/plain".to_vec()),
                    (
                        "set-cookie".to_string(),
                        b"sid=preexisting; Path=/; HttpOnly".to_vec(),
                    ),
                ],
                Bytes::from_static(b"cached-legacy-body"),
            ),
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();

    let pre_layer = CacheLayer::builder(pre_backend)
        .ttl(Duration::from_secs(60))
        .build();
    let mut pre_svc = pre_layer.layer(service_fn({
        let calls = pre_calls.clone();
        move |_req: Request<()>| {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { Ok::<_, Infallible>(http::Response::new(Full::from("origin"))) }
        }
    }));

    let pre_hit = pre_svc
        .ready()
        .await
        .unwrap()
        .call(Request::get("/legacy").body(()).unwrap())
        .await
        .unwrap();
    assert_eq!(pre_hit.headers().get("content-type").unwrap(), "text/plain");
    assert!(
        pre_hit.headers().get(http::header::SET_COOKIE).is_none(),
        "cached hit from pre-stored entry must strip Set-Cookie"
    );
    let pre_body = pre_hit.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&pre_body[..], b"cached-legacy-body");
    assert_eq!(pre_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pre_stored_entry_strips_set_cookie_and_hop_by_hop_headers_on_hit() {
    let calls = Arc::new(AtomicUsize::new(0));
    let backend = InMemoryBackend::new(64);
    backend
        .set(
            "/persisted".to_string(),
            CacheEntry::new(
                StatusCode::OK,
                Version::HTTP_11,
                vec![
                    ("content-type".to_string(), b"application/json".to_vec()),
                    ("Set-Cookie".to_string(), b"sid=old-1; HttpOnly".to_vec()),
                    ("set-cookie".to_string(), b"sid=old-2; Secure".to_vec()),
                    (
                        "Connection".to_string(),
                        b"keep-alive, x-hop-custom".to_vec(),
                    ),
                    ("Keep-Alive".to_string(), b"timeout=5".to_vec()),
                    ("Transfer-Encoding".to_string(), b"chunked".to_vec()),
                    ("Proxy-Authenticate".to_string(), b"Basic".to_vec()),
                    ("X-Hop-Custom".to_string(), b"internal".to_vec()),
                ],
                Bytes::from_static(br#"{"ok":true}"#),
            ),
            Duration::from_secs(60),
            Duration::ZERO,
        )
        .await
        .unwrap();

    let policy = CachePolicy::default()
        .with_ttl(Duration::from_secs(60))
        .with_streaming_policy(StreamingPolicy {
            enable_chunk_cache: true,
            chunk_size: 4,
            min_chunk_file_size: 4,
            excluded_content_types: HashSet::new(),
            range_handling: RangeHandling::CacheFullServeRanges,
            ..Default::default()
        });
    let layer = CacheLayer::builder(backend).policy(policy).build();
    let chunks = layer.chunk_cache().expect("chunk cache enabled").clone();
    let mut svc = layer.layer(service_fn({
        let calls = calls.clone();
        move |_req: Request<()>| {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { Ok::<_, Infallible>(http::Response::new(Full::from("origin"))) }
        }
    }));

    // 1. Regular backend hit strips Set-Cookie, hop-by-hop, and Connection-nominated headers.
    let hit = svc
        .ready()
        .await
        .unwrap()
        .call(Request::get("/persisted").body(()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        hit.headers().get("content-type").unwrap(),
        "application/json"
    );
    for forbidden in [
        "set-cookie",
        "connection",
        "keep-alive",
        "transfer-encoding",
        "proxy-authenticate",
        "x-hop-custom",
    ] {
        assert!(
            hit.headers().get(forbidden).is_none(),
            "cached hit must strip {forbidden}"
        );
    }
    let body = hit.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], br#"{"ok":true}"#);
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    // 2. Chunk cache range hit also strips Set-Cookie and hop-by-hop headers.
    chunks.replace_with_ttl(
        "/chunked".to_string(),
        ChunkMetadata {
            total_size: 8,
            content_type: "text/plain".into(),
            etag: None,
            last_modified: None,
            status: StatusCode::OK,
            version: Version::HTTP_11,
            headers: vec![
                ("content-type".into(), b"text/plain".to_vec()),
                ("Set-Cookie".into(), b"sid=chunk-secret".to_vec()),
                ("Connection".into(), b"x-chunk-hop".to_vec()),
                ("x-chunk-hop".into(), b"hop-val".to_vec()),
                ("transfer-encoding".into(), b"chunked".to_vec()),
            ],
        },
        Some(Duration::from_secs(60)),
        vec![Bytes::from_static(b"ABCD"), Bytes::from_static(b"EFGH")],
    );

    let range_hit = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/chunked")
                .header(http::header::RANGE, "bytes=0-3")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(range_hit.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        range_hit.headers().get("content-type").unwrap(),
        "text/plain"
    );
    for forbidden in [
        "set-cookie",
        "connection",
        "x-chunk-hop",
        "transfer-encoding",
    ] {
        assert!(
            range_hit.headers().get(forbidden).is_none(),
            "chunk hit must strip {forbidden}"
        );
    }
    let range_body = range_hit.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&range_body[..], b"ABCD");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
