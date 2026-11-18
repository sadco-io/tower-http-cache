use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http::header::CACHE_CONTROL;
use http::{HeaderValue, Request, StatusCode};
use http_body::Frame;
use http_body_util::StreamBody;
use http_body_util::{BodyExt, Full};
use std::convert::Infallible;
use tokio::sync::Notify;
use tokio::time::sleep;
use tower::service_fn;
use tower::{Layer, Service, ServiceExt};
use tower_http_cache::backend::CacheRead;
use tower_http_cache::backend::memory::InMemoryBackend;
use tower_http_cache::error::CacheError;
use tower_http_cache::prelude::*;

#[tokio::test]
async fn caches_successful_gets() {
    let backend = InMemoryBackend::new(128);
    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(1))
        .build();

    let counter = Arc::new(AtomicUsize::new(0));

    let handler = service_fn({
        let counter = counter.clone();
        move |_req: Request<()>| {
            let counter = counter.clone();
            async move {
                let value = counter.fetch_add(1, Ordering::SeqCst) + 1;
                Ok::<_, std::convert::Infallible>(http::Response::new(Full::from(
                    value.to_string(),
                )))
            }
        }
    });

    let mut service = layer.layer(handler);
    service.ready().await.expect("service ready");
    let first = service
        .call(Request::new(()))
        .await
        .expect(
            "fir
        st call succeeds",
        )
        .into_body()
        .collect()
        .await
        .expect("first body collected")
        .to_bytes();
    let first = String::from_utf8(first.to_vec()).expect("first body utf-8");
    service.ready().await.expect("service ready");
    let second = service
        .call(Request::new(()))
        .await
        .expect("second call succeeds")
        .into_body()
        .collect()
        .await
        .expect("second body collected")
        .to_bytes();
    let second = String::from_utf8(second.to_vec()).expect("second body utf-8");

    assert_eq!(first, "1");
    assert_eq!(second, "1", "second response should come from cache");
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn respects_cache_control_bypass() {
    let backend = InMemoryBackend::new(128);
    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(1))
        .build();

    let counter = Arc::new(AtomicUsize::new(0));

    let mut service = layer.layer(service_fn({
        let counter = counter.clone();
        move |req: Request<()>| {
            let counter = counter.clone();
            async move {
                assert!(
                    req.headers()
                        .get(CACHE_CONTROL)
                        .is_some_and(|value| value == "no-cache"),
                    "request should propagate cache-control header"
                );
                let value = counter.fetch_add(1, Ordering::SeqCst) + 1;
                Ok::<_, std::convert::Infallible>(http::Response::new(Full::from(
                    value.to_string(),
                )))
            }
        }
    }));

    let mut request = Request::new(());
    request
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));

    service.ready().await.expect("service ready");
    let first = service
        .call(request.clone())
        .await
        .expect("first call succeeds")
        .into_body()
        .collect()
        .await
        .expect("first body collected")
        .to_bytes();
    let first = String::from_utf8(first.to_vec()).expect("first body utf-8");
    service.ready().await.expect("service ready");
    let second = service
        .call(request)
        .await
        .expect("second call succeeds")
        .into_body()
        .collect()
        .await
        .expect("second body collected")
        .to_bytes();
    let second = String::from_utf8(second.to_vec()).expect("second body utf-8");

    assert_eq!(first, "1");
    assert_eq!(second, "2");
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn caches_negative_responses_for_negative_ttl() {
    let backend = InMemoryBackend::new(128);
    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(1))
        .statuses([200_u16, 203, 300, 301])
        .negative_ttl(Duration::from_millis(150))
        .build();

    let counter = Arc::new(AtomicUsize::new(0));

    let mut service = layer.layer(service_fn({
        let counter = counter.clone();
        move |_req: Request<()>| {
            let counter = counter.clone();
            async move {
                let value = counter.fetch_add(1, Ordering::SeqCst) + 1;
                let mut response = http::Response::new(Full::from(format!("not-found-{value}")));
                *response.status_mut() = StatusCode::NOT_FOUND;
                Ok::<_, std::convert::Infallible>(response)
            }
        }
    }));

    service.ready().await.expect("service ready");
    let first = service
        .call(Request::new(()))
        .await
        .expect("first call succeeds")
        .into_body()
        .collect()
        .await
        .expect("first body collected")
        .to_bytes();
    let first = String::from_utf8(first.to_vec()).expect("first body utf-8");
    service.ready().await.expect("service ready");
    let second = service
        .call(Request::new(()))
        .await
        .expect("second call succeeds")
        .into_body()
        .collect()
        .await
        .expect("second body collected")
        .to_bytes();
    let second = String::from_utf8(second.to_vec()).expect("second body utf-8");
    assert_eq!(first, "not-found-1");
    assert_eq!(second, "not-found-1");
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    sleep(Duration::from_millis(260)).await;

    service.ready().await.expect("service ready");
    let third = service
        .call(Request::new(()))
        .await
        .expect("third call succeeds")
        .into_body()
        .collect()
        .await
        .expect("third body collected")
        .to_bytes();
    let third = String::from_utf8(third.to_vec()).expect("third body utf-8");
    assert_eq!(third, "not-found-2");
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_requests_share_refresh_work() {
    let backend = InMemoryBackend::new(128);
    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_millis(50))
        .stale_while_revalidate(Duration::from_millis(300))
        .refresh_before(Duration::from_millis(20))
        .build();

    let counter = Arc::new(AtomicUsize::new(0));

    let handler = service_fn({
        let counter = counter.clone();
        move |_req: Request<()>| {
            let counter = counter.clone();
            async move {
                let value = counter.fetch_add(1, Ordering::SeqCst) + 1;
                Ok::<_, std::convert::Infallible>(http::Response::new(Full::from(
                    value.to_string(),
                )))
            }
        }
    });

    // Warm cache
    let mut warm_service = layer.clone().layer(handler.clone());
    warm_service.ready().await.expect("warm service ready");
    let warm_body = warm_service
        .call(Request::new(()))
        .await
        .expect("warm call succeeds")
        .into_body()
        .collect()
        .await
        .expect("warm body collected")
        .to_bytes();
    let warm_body = String::from_utf8(warm_body.to_vec()).expect("warm body utf-8");
    assert_eq!(warm_body, "1");

    sleep(Duration::from_millis(80)).await;

    let layer_a = layer.clone();
    let handler_a = handler.clone();
    let layer_b = layer;
    let handler_b = handler;

    let task_a = tokio::spawn(async move {
        let mut svc = layer_a.layer(handler_a);
        svc.ready().await.expect("service ready");
        svc.call(Request::new(()))
            .await
            .expect("call succeeds")
            .into_body()
            .collect()
            .await
            .expect("body collected")
            .to_bytes()
            .to_vec()
    });

    let task_b = tokio::spawn(async move {
        let mut svc = layer_b.layer(handler_b);
        svc.ready().await.expect("service ready");
        svc.call(Request::new(()))
            .await
            .expect("call succeeds")
            .into_body()
            .collect()
            .await
            .expect("body collected")
            .to_bytes()
            .to_vec()
    });

    let resp_a = task_a.await.expect("task a join succeeded");
    let resp_b = task_b.await.expect("task b join succeeded");

    let resp_a = String::from_utf8(resp_a).expect("valid utf-8 body");
    let resp_b = String::from_utf8(resp_b).expect("valid utf-8 body");

    let bodies = [resp_a.as_str(), resp_b.as_str()];

    // Both racers must see a body the cache legitimately holds. Which one each
    // observes is *not* part of the stale-while-revalidate contract: whether a
    // given caller gets the stale entry or the already-refreshed one depends on
    // how the two tasks interleave with the background refresh. Asserting one of
    // each made this test ~50% flaky.
    assert!(
        bodies.iter().all(|b| *b == "1" || *b == "2"),
        "each response should be either the stale or the refreshed body: got {bodies:?}"
    );

    // This is the actual contract, and it is deterministic: the origin is hit
    // exactly twice -- once to warm the cache, once for the single coalesced
    // refresh. A third hit would mean the two concurrent requests each kicked
    // off their own refresh instead of sharing one.
    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "concurrent requests must share one refresh, not trigger one each"
    );
}

/// The hole that made `concurrent_requests_share_refresh_work` flaky, forced
/// rather than raced for -- on a cold miss, so no clock is involved.
///
/// A caller's cache lookup and its stampede-lock acquisition are two separate
/// steps. This test parks the second caller between them, for exactly as long
/// as it takes the first caller to reach the origin, store the response and
/// release the lock. The second caller therefore wakes holding a lookup that
/// predates a store that has already happened, and finds the lock free. Before
/// the double-checked read in `CacheService::call` it went on to call the
/// origin itself; the origin counter is the assertion that it no longer does.
///
/// Nothing here depends on timing: every step waits for the previous one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_lock_acquisition_does_not_refetch() {
    /// One-shot rendezvous. Arm it, and the next party to reach it announces
    /// its arrival and blocks until released. Disarms on first use, so a
    /// caller that passes the same point twice is only caught once.
    #[derive(Default)]
    struct Gate {
        armed: AtomicBool,
        arrived: Notify,
        release: Notify,
    }

    impl Gate {
        fn arm(&self) {
            self.armed.store(true, Ordering::SeqCst);
        }

        async fn pass(&self) {
            if self.armed.swap(false, Ordering::SeqCst) {
                self.arrived.notify_one();
                self.release.notified().await;
            }
        }

        async fn wait_for_arrival(&self) {
            self.arrived.notified().await;
        }

        fn release(&self) {
            self.release.notify_one();
        }
    }

    /// Passes through to an `InMemoryBackend`, with a gate sitting between a
    /// lookup returning and its caller acting on the result.
    #[derive(Clone)]
    struct GatedBackend {
        inner: InMemoryBackend,
        after_get: Arc<Gate>,
    }

    impl CacheBackend for GatedBackend {
        async fn get(&self, key: &str) -> Result<Option<CacheRead>, CacheError> {
            let read = self.inner.get(key).await;
            self.after_get.pass().await;
            read
        }

        async fn set(
            &self,
            key: String,
            entry: CacheEntry,
            ttl: Duration,
            stale_for: Duration,
        ) -> Result<(), CacheError> {
            self.inner.set(key, entry, ttl, stale_for).await
        }

        async fn invalidate(&self, key: &str) -> Result<(), CacheError> {
            self.inner.invalidate(key).await
        }
    }

    let after_get = Arc::new(Gate::default());
    let in_origin = Arc::new(Gate::default());

    let layer = CacheLayer::builder(GatedBackend {
        inner: InMemoryBackend::new(128),
        after_get: after_get.clone(),
    })
    .ttl(Duration::from_secs(300))
    .build();

    let counter = Arc::new(AtomicUsize::new(0));
    let handler = service_fn({
        let counter = counter.clone();
        let in_origin = in_origin.clone();
        move |_req: Request<()>| {
            let counter = counter.clone();
            let in_origin = in_origin.clone();
            async move {
                let value = counter.fetch_add(1, Ordering::SeqCst) + 1;
                in_origin.pass().await;
                Ok::<_, Infallible>(http::Response::new(Full::from(value.to_string())))
            }
        }
    });

    // First caller: misses, takes the lock, parks inside the origin call.
    in_origin.arm();
    let layer_first = layer.clone();
    let handler_first = handler.clone();
    let first = tokio::spawn(async move {
        let mut svc = layer_first.layer(handler_first);
        svc.ready().await.expect("service ready");
        svc.call(Request::new(()))
            .await
            .expect("call succeeds")
            .into_body()
            .collect()
            .await
            .expect("body collected")
            .to_bytes()
    });
    in_origin.wait_for_arrival().await;

    // Second caller: misses too -- the first caller has stored nothing yet --
    // and parks between that lookup and the lock.
    after_get.arm();
    let second = tokio::spawn(async move {
        let mut svc = layer.layer(handler);
        svc.ready().await.expect("service ready");
        svc.call(Request::new(()))
            .await
            .expect("call succeeds")
            .into_body()
            .collect()
            .await
            .expect("body collected")
            .to_bytes()
    });
    after_get.wait_for_arrival().await;

    // Let the first caller run to completion: store, release the lock, return.
    in_origin.release();
    let first_body = first.await.expect("first task joined");
    assert_eq!(first_body.as_ref(), b"1");

    // Only now does the second caller go for the lock, which is free.
    after_get.release();
    let second_body = second.await.expect("second task joined");

    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "second caller re-fetched a key that was populated while it waited"
    );
    assert_eq!(
        second_body.as_ref(),
        b"1",
        "second caller served a body the cache never held"
    );
}

#[tokio::test]
async fn header_allowlist_filters_cached_headers() {
    let backend = InMemoryBackend::new(128);
    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(1))
        .header_allowlist(["x-allowed"])
        .build();

    let counter = Arc::new(AtomicUsize::new(0));

    let handler = service_fn({
        let counter = counter.clone();
        move |_req: Request<()>| {
            let counter = counter.clone();
            async move {
                let value = counter.fetch_add(1, Ordering::SeqCst) + 1;
                let mut response = http::Response::new(Full::from(value.to_string()));
                response
                    .headers_mut()
                    .insert("x-allowed", HeaderValue::from_static("yes"));
                response
                    .headers_mut()
                    .insert("x-blocked", HeaderValue::from_static("no"));
                Ok::<_, Infallible>(response)
            }
        }
    });

    let mut service = layer.layer(handler);

    service.ready().await.expect("service ready");
    let first = service
        .call(Request::new(()))
        .await
        .expect("first call succeeds");
    let (first_parts, first_body) = first.into_parts();
    assert_eq!(
        first_parts.headers.get("x-allowed").unwrap(),
        &HeaderValue::from_static("yes")
    );
    assert_eq!(
        first_parts.headers.get("x-blocked").unwrap(),
        &HeaderValue::from_static("no")
    );
    let first_bytes = first_body
        .collect()
        .await
        .expect("first body collected")
        .to_bytes();
    let first_text = String::from_utf8(first_bytes.to_vec()).expect("first body utf-8");
    assert_eq!(first_text, "1");

    service.ready().await.expect("service ready");
    let cached = service
        .call(Request::new(()))
        .await
        .expect("cached call succeeds");
    let (cached_parts, cached_body) = cached.into_parts();
    assert_eq!(
        cached_parts.headers.get("x-allowed").unwrap(),
        &HeaderValue::from_static("yes")
    );
    assert!(
        cached_parts.headers.get("x-blocked").is_none(),
        "blocked header should not be cached"
    );
    let cached_bytes = cached_body
        .collect()
        .await
        .expect("cached body collected")
        .to_bytes();
    let cached_text = String::from_utf8(cached_bytes.to_vec()).expect("cached body utf-8");
    assert_eq!(cached_text, "1");
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn max_body_size_prevents_caching() {
    let backend = InMemoryBackend::new(128);
    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(1))
        .max_body_size(Some(4))
        .build();

    let counter = Arc::new(AtomicUsize::new(0));

    let mut service = layer.layer(service_fn({
        let counter = counter.clone();
        move |_req: Request<()>| {
            let counter = counter.clone();
            async move {
                let value = counter.fetch_add(1, Ordering::SeqCst) + 1;
                let body = format!("large-body-{value}");
                Ok::<_, Infallible>(http::Response::new(Full::from(body)))
            }
        }
    }));

    service.ready().await.expect("service ready");
    let first = service
        .call(Request::new(()))
        .await
        .expect("first call succeeds")
        .into_body()
        .collect()
        .await
        .expect("first body collected")
        .to_bytes();
    let first_text = String::from_utf8(first.to_vec()).expect("first body utf-8");

    service.ready().await.expect("service ready");
    let second = service
        .call(Request::new(()))
        .await
        .expect("second call succeeds")
        .into_body()
        .collect()
        .await
        .expect("second body collected")
        .to_bytes();
    let second_text = String::from_utf8(second.to_vec()).expect("second body utf-8");

    assert_eq!(first_text, "large-body-1");
    assert_eq!(second_text, "large-body-2");
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn min_body_size_prevents_caching() {
    let backend = InMemoryBackend::new(128);
    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(1))
        .min_body_size(Some(16))
        .build();

    let counter = Arc::new(AtomicUsize::new(0));

    let mut service = layer.layer(service_fn({
        let counter = counter.clone();
        move |_req: Request<()>| {
            let counter = counter.clone();
            async move {
                let value = counter.fetch_add(1, Ordering::SeqCst) + 1;
                let body = format!("tiny-{value}");
                Ok::<_, Infallible>(http::Response::new(Full::from(body)))
            }
        }
    }));

    service.ready().await.expect("service ready");
    let first = service
        .call(Request::new(()))
        .await
        .expect("first call succeeds")
        .into_body()
        .collect()
        .await
        .expect("first body collected")
        .to_bytes();
    let first_text = String::from_utf8(first.to_vec()).expect("first body utf-8");

    service.ready().await.expect("service ready");
    let second = service
        .call(Request::new(()))
        .await
        .expect("second call succeeds")
        .into_body()
        .collect()
        .await
        .expect("second body collected")
        .to_bytes();
    let second_text = String::from_utf8(second.to_vec()).expect("second body utf-8");

    assert_eq!(first_text, "tiny-1");
    assert_eq!(second_text, "tiny-2");
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn streaming_bodies_not_cached_when_disabled() {
    let backend = InMemoryBackend::new(128);
    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(1))
        .allow_streaming_bodies(false)
        .build();

    let counter = Arc::new(AtomicUsize::new(0));

    let mut service = layer.layer(service_fn({
        let counter = counter.clone();
        move |_req: Request<()>| {
            let counter = counter.clone();
            async move {
                let value = counter.fetch_add(1, Ordering::SeqCst) + 1;
                let payload = format!("stream-{value}");
                let stream = futures_util::stream::unfold(
                    Some(Bytes::from(payload.into_bytes())),
                    |state| async move {
                        state.map(|bytes| {
                            (
                                Ok::<Frame<Bytes>, Infallible>(Frame::data(bytes.clone())),
                                None,
                            )
                        })
                    },
                );
                let body = StreamBody::new(stream);
                Ok::<_, Infallible>(http::Response::new(body))
            }
        }
    }));

    service.ready().await.expect("service ready");
    let first = service
        .call(Request::new(()))
        .await
        .expect("first call succeeds")
        .into_body()
        .collect()
        .await
        .expect("first body collected")
        .to_bytes();
    let first_text = String::from_utf8(first.to_vec()).expect("first body utf-8");

    service.ready().await.expect("service ready");
    let second = service
        .call(Request::new(()))
        .await
        .expect("second call succeeds")
        .into_body()
        .collect()
        .await
        .expect("second body collected")
        .to_bytes();
    let second_text = String::from_utf8(second.to_vec()).expect("second body utf-8");

    assert_eq!(first_text, "stream-1");
    assert_eq!(second_text, "stream-2");
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cache_respects_disabled_cache_control() {
    let backend = InMemoryBackend::new(128);
    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(1))
        .respect_cache_control(false)
        .build();

    let counter = Arc::new(AtomicUsize::new(0));

    let mut service = layer.layer(service_fn({
        let counter = counter.clone();
        move |req: Request<()>| {
            let counter = counter.clone();
            async move {
                assert!(
                    req.headers()
                        .get(CACHE_CONTROL)
                        .is_some_and(|value| value == "no-cache"),
                    "header should propagate to inner service"
                );
                let value = counter.fetch_add(1, Ordering::SeqCst) + 1;
                Ok::<_, Infallible>(http::Response::new(Full::from(value.to_string())))
            }
        }
    }));

    let mut request = Request::new(());
    request
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));

    service.ready().await.expect("service ready");
    let first = service
        .call(request.clone())
        .await
        .expect("first call succeeds")
        .into_body()
        .collect()
        .await
        .expect("first body collected")
        .to_bytes();
    let first = String::from_utf8(first.to_vec()).expect("first body utf-8");

    service.ready().await.expect("service ready");
    let cached = service
        .call(request)
        .await
        .expect("cached call succeeds")
        .into_body()
        .collect()
        .await
        .expect("cached body collected")
        .to_bytes();
    let cached = String::from_utf8(cached.to_vec()).expect("cached body utf-8");

    assert_eq!(first, "1");
    assert_eq!(cached, "1");
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn custom_key_extractor_collapses_query_variants() {
    let backend = InMemoryBackend::new(128);
    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(1))
        .key_extractor(KeyExtractor::path())
        .build();

    let counter = Arc::new(AtomicUsize::new(0));

    let mut service = layer.layer(service_fn({
        let counter = counter.clone();
        move |req: Request<()>| {
            let counter = counter.clone();
            async move {
                let _ = req;
                let value = counter.fetch_add(1, Ordering::SeqCst) + 1;
                let mut response = http::Response::new(Full::from(value.to_string()));
                *response.status_mut() = StatusCode::OK;
                Ok::<_, Infallible>(response)
            }
        }
    }));

    let first_request = Request::builder()
        .uri("/resource?variant=1")
        .body(())
        .expect("request builder");
    service.ready().await.expect("service ready");
    let first = service
        .call(first_request)
        .await
        .expect("first call succeeds")
        .into_body()
        .collect()
        .await
        .expect("first body collected")
        .to_bytes();
    let first = String::from_utf8(first.to_vec()).expect("first body utf-8");

    let second_request = Request::builder()
        .uri("/resource?variant=2")
        .body(())
        .expect("request builder");
    service.ready().await.expect("service ready");
    let cached = service
        .call(second_request)
        .await
        .expect("cached call succeeds")
        .into_body()
        .collect()
        .await
        .expect("cached body collected")
        .to_bytes();
    let cached = String::from_utf8(cached.to_vec()).expect("cached body utf-8");

    assert_eq!(first, "1");
    assert_eq!(cached, "1");
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[cfg(feature = "compression")]
#[tokio::test]
async fn compression_caches_gzip_payloads() {
    use flate2::read::GzDecoder;
    use std::io::Read;
    use tower_http_cache::policy::{CompressionConfig, CompressionStrategy};

    let backend = InMemoryBackend::new(128);
    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(1))
        .compression(CompressionConfig {
            strategy: CompressionStrategy::Gzip,
            min_size: 0,
        })
        .build();

    let counter = Arc::new(AtomicUsize::new(0));
    let payload = "payload-to-compress".repeat(8);

    let mut service = layer.layer(service_fn({
        let counter = counter.clone();
        let payload = payload.clone();
        move |_req: Request<()>| {
            let counter = counter.clone();
            let payload = payload.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok::<_, Infallible>(http::Response::new(Full::from(payload)))
            }
        }
    }));

    service.ready().await.expect("service ready");
    let first_resp = service
        .call(
            Request::builder()
                .header(http::header::ACCEPT_ENCODING, "gzip")
                .body(())
                .unwrap(),
        )
        .await
        .expect("first call succeeds");
    assert_eq!(
        first_resp
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        Some("gzip"),
        "miss response for gzip-capable client must be gzip encoded"
    );
    let first = first_resp
        .into_body()
        .collect()
        .await
        .expect("first body collected")
        .to_bytes();
    let mut decoder = GzDecoder::new(&first[..]);
    let mut first_text = String::new();
    decoder
        .read_to_string(&mut first_text)
        .expect("first gzip decode succeeds");
    assert_eq!(first_text, payload);

    service.ready().await.expect("service ready");
    let cached_response = service
        .call(
            Request::builder()
                .header(http::header::ACCEPT_ENCODING, "gzip")
                .body(())
                .unwrap(),
        )
        .await
        .expect("cached call succeeds");
    assert_eq!(
        cached_response
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        Some("gzip"),
        "compressed cached response must set Content-Encoding: gzip when client accepts gzip"
    );
    assert!(
        cached_response
            .headers()
            .get_all(http::header::VARY)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|v| v.to_ascii_lowercase().contains("accept-encoding")),
        "compressed cached response must include Vary: Accept-Encoding"
    );
    let advertised_len: usize = cached_response
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .expect("compressed cached response must update Content-Length");

    let cached = cached_response
        .into_body()
        .collect()
        .await
        .expect("cached body collected")
        .to_bytes();
    assert_eq!(advertised_len, cached.len());
    assert_ne!(
        cached,
        Bytes::copy_from_slice(payload.as_bytes()),
        "cached bytes should be gzip encoded"
    );

    let mut decoder = GzDecoder::new(&cached[..]);
    let mut decompressed = String::new();
    decoder
        .read_to_string(&mut decompressed)
        .expect("gzip decode succeeds");
    assert_eq!(decompressed, payload);

    // Client that does not accept gzip must receive identity bytes from the same cached entry.
    service.ready().await.expect("service ready");
    let identity_resp = service
        .call(Request::new(()))
        .await
        .expect("identity cached call succeeds");
    assert!(
        identity_resp
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .is_none(),
        "identity client must not receive Content-Encoding: gzip"
    );
    let identity_body = identity_resp
        .into_body()
        .collect()
        .await
        .expect("identity body collected")
        .to_bytes();
    assert_eq!(identity_body, Bytes::copy_from_slice(payload.as_bytes()));
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[cfg(feature = "compression")]
#[tokio::test]
async fn compression_skips_pre_encoded_upstream_responses() {
    use tower_http_cache::policy::{CompressionConfig, CompressionStrategy};

    let backend = InMemoryBackend::new(128);
    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(10))
        .compression(CompressionConfig {
            strategy: CompressionStrategy::Gzip,
            min_size: 0,
        })
        .build();

    let pre_encoded = Bytes::from_static(b"synthetic-brotli-payload-bytes-1234567890");
    let expected_len = pre_encoded.len();

    let mut service = layer.layer(service_fn({
        let pre_encoded = pre_encoded.clone();
        move |_req: Request<()>| {
            let body = pre_encoded.clone();
            async move {
                Ok::<_, Infallible>(
                    http::Response::builder()
                        .header(http::header::CONTENT_ENCODING, "br")
                        .header(http::header::CONTENT_LENGTH, body.len().to_string())
                        .body(Full::from(body))
                        .expect("response builder"),
                )
            }
        }
    }));

    service.ready().await.expect("service ready");
    let _first = service
        .call(Request::new(()))
        .await
        .expect("first call succeeds")
        .into_body()
        .collect()
        .await
        .expect("first body");

    service.ready().await.expect("service ready");
    let cached_response = service
        .call(Request::new(()))
        .await
        .expect("cached call succeeds");
    assert_eq!(
        cached_response
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        Some("br"),
        "pre-encoded response must keep its original Content-Encoding"
    );
    assert_eq!(
        cached_response
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok()),
        Some(expected_len),
    );
    let cached_body = cached_response
        .into_body()
        .collect()
        .await
        .expect("cached body")
        .to_bytes();
    assert_eq!(cached_body, pre_encoded);
}

#[cfg(feature = "admin-api")]
#[tokio::test]
async fn admin_state_receives_live_layer_statistics_and_invalidations() {
    use tower_http_cache::admin::routes::{
        InvalidationRequest, handle_hot_keys, handle_invalidate, handle_stats,
    };
    use tower_http_cache::admin::{AdminConfig, AdminState};

    let backend = InMemoryBackend::new(64);
    let layer = CacheLayer::builder(backend.clone())
        .ttl(Duration::from_secs(60))
        .build();
    let admin_state = AdminState::with_stats(backend, AdminConfig::new(), layer.stats());

    let mut service = layer.layer(service_fn(|_req: Request<()>| async move {
        Ok::<_, Infallible>(http::Response::new(Full::from("admin-observed")))
    }));

    // 1 miss + 1 store
    let req1 = Request::builder().uri("/items/1").body(()).unwrap();
    let _ = service
        .ready()
        .await
        .unwrap()
        .call(req1)
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap();

    // 2 hits
    for _ in 0..2 {
        let req = Request::builder().uri("/items/1").body(()).unwrap();
        let _ = service
            .ready()
            .await
            .unwrap()
            .call(req)
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap();
    }

    let stats_resp = handle_stats(&admin_state);
    assert_eq!(stats_resp.cache.misses, 1);
    assert_eq!(stats_resp.cache.stores, 1);
    assert_eq!(stats_resp.cache.hits, 2);
    assert_eq!(stats_resp.cache.total_requests, 3);

    let hot = handle_hot_keys(&admin_state, Some(5));
    assert_eq!(hot.hot_keys.len(), 1);
    assert_eq!(hot.hot_keys[0].key, "/items/1");
    assert_eq!(hot.hot_keys[0].hits, 2);

    // Layer invalidation increments GlobalStats
    layer.invalidate("/items/1").await.unwrap();
    let after_layer_inv = handle_stats(&admin_state);
    assert_eq!(after_layer_inv.cache.invalidations, 1);

    // Admin handler invalidation also increments shared GlobalStats
    let inv_resp = handle_invalidate(
        &admin_state,
        InvalidationRequest {
            key: Some("/items/other".to_string()),
            tag: None,
            tags: None,
        },
    )
    .await
    .unwrap();
    assert!(inv_resp.success);
    assert_eq!(handle_stats(&admin_state).cache.invalidations, 2);
}

#[cfg(feature = "serde")]
#[tokio::test]
async fn ml_logging_records_hit_miss_store_and_bypass_without_errors() {
    use tower_http_cache::logging::MLLoggingConfig;
    use tower_http_cache::policy::CachePolicy;

    let backend = InMemoryBackend::new(64);
    let policy = CachePolicy::default()
        .with_ttl(Duration::from_secs(60))
        .with_ml_logging(
            MLLoggingConfig::new()
                .with_enabled(true)
                .with_sample_rate(1.0)
                .with_hash_keys(false),
        );

    let layer = CacheLayer::builder(backend).policy(policy).build();
    let hits = Arc::new(AtomicUsize::new(0));
    let mut service = layer.layer(service_fn({
        let hits = hits.clone();
        move |_req: Request<()>| {
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                Ok::<_, Infallible>(http::Response::new(Full::from("ml-logged")))
            }
        }
    }));

    // Miss + store with explicit X-Request-ID
    let req1 = Request::builder()
        .uri("/ml-test")
        .header("x-request-id", "req-abc-123")
        .body(())
        .unwrap();
    let _ = service
        .ready()
        .await
        .unwrap()
        .call(req1)
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap();

    // Hit
    let req2 = Request::builder().uri("/ml-test").body(()).unwrap();
    let _ = service
        .ready()
        .await
        .unwrap()
        .call(req2)
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap();

    // Bypass (POST)
    let req3 = Request::builder()
        .method(http::Method::POST)
        .uri("/ml-test")
        .body(())
        .unwrap();
    let _ = service
        .ready()
        .await
        .unwrap()
        .call(req3)
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap();

    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn axum_router_caches_with_handle_error_layer() {
    use axum::{Router, body::Body, error_handling::HandleErrorLayer, routing::get};
    use tower::{ServiceBuilder, ServiceExt};
    use tower_http_cache::layer::BoxError;

    let backend = InMemoryBackend::new(64);
    let cache_layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(60))
        .build();

    let hits = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/hello",
            get({
                let hits = hits.clone();
                move || {
                    let hits = hits.clone();
                    async move {
                        let count = hits.fetch_add(1, Ordering::SeqCst) + 1;
                        format!("hit-{count}")
                    }
                }
            }),
        )
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(|err: BoxError| async move {
                    (StatusCode::INTERNAL_SERVER_ERROR, format!("err: {err}"))
                }))
                .layer(cache_layer),
        );

    let r1 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/hello")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let b1 = r1.into_body().collect().await.unwrap().to_bytes();

    let r2 = app
        .oneshot(
            Request::builder()
                .uri("/hello")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let b2 = r2.into_body().collect().await.unwrap().to_bytes();

    assert_eq!(&b1[..], b"hit-1");
    assert_eq!(&b2[..], b"hit-1");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[cfg(feature = "compression")]
#[tokio::test]
async fn compression_negotiates_accept_encoding_and_etag_untagging() {
    use tower_http_cache::policy::{CompressionConfig, CompressionStrategy};

    let layer = CacheLayer::builder(InMemoryBackend::new(100))
        .compression(CompressionConfig {
            strategy: CompressionStrategy::Gzip,
            min_size: 16,
        })
        .build();
    let body = "x".repeat(4096);
    let calls = Arc::new(AtomicUsize::new(0));
    let mut svc = layer.layer(service_fn({
        let calls = calls.clone();
        move |_r: Request<()>| {
            calls.fetch_add(1, Ordering::SeqCst);
            let b = body.clone();
            async move {
                Ok::<_, Infallible>(
                    http::Response::builder()
                        .header("content-type", "text/plain")
                        .header("etag", "\"v1\"")
                        .body(Full::from(b))
                        .unwrap(),
                )
            }
        }
    }));

    // Miss for identity client receives uncompressed body + Vary: Accept-Encoding.
    let miss_resp = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/c")
                .header(http::header::ACCEPT_ENCODING, "identity")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        miss_resp
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .is_none()
    );
    assert_eq!(
        miss_resp.headers().get(http::header::ETAG).unwrap(),
        "\"v1\""
    );
    assert!(miss_resp.headers().get(http::header::VARY).is_some());
    let miss_bytes = miss_resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(miss_bytes.len(), 4096);

    // Hit for `gzip;q=0` receives decompressed body, no Content-Encoding, untagged ETag.
    let q0_resp = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/c")
                .header(http::header::ACCEPT_ENCODING, "gzip;q=0")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        q0_resp
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .is_none()
    );
    assert_eq!(q0_resp.headers().get(http::header::ETAG).unwrap(), "\"v1\"");
    let q0_bytes = q0_resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(q0_bytes.len(), 4096);

    // Hit for `gzip` receives compressed body and `-gzip` ETag.
    let gz_resp = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/c")
                .header(http::header::ACCEPT_ENCODING, "gzip")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        gz_resp
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .unwrap(),
        "gzip"
    );
    assert_eq!(
        gz_resp.headers().get(http::header::ETAG).unwrap(),
        "\"v1-gzip\""
    );
    let gz_bytes = gz_resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&gz_bytes[..2], &[0x1f, 0x8b]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[cfg(feature = "compression")]
#[tokio::test]
async fn allowlist_preserves_upstream_content_encoding_and_vary() {
    use std::io::Write;

    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(b"hello hello hello").unwrap();
    let gz = Bytes::from(enc.finish().unwrap());

    let layer = CacheLayer::builder(InMemoryBackend::new(100))
        .header_allowlist(["content-type"])
        .build();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut svc = layer.layer(service_fn({
        let calls = calls.clone();
        move |_r: Request<()>| {
            calls.fetch_add(1, Ordering::SeqCst);
            let gz = gz.clone();
            async move {
                Ok::<_, Infallible>(
                    http::Response::builder()
                        .header("content-type", "text/plain")
                        .header("content-encoding", "gzip")
                        .header("vary", "Accept-Encoding")
                        .body(Full::from(gz))
                        .unwrap(),
                )
            }
        }
    }));

    let _ = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/g")
                .header(http::header::ACCEPT_ENCODING, "gzip")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap();

    let hit = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/g")
                .header(http::header::ACCEPT_ENCODING, "gzip")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        hit.headers().get(http::header::CONTENT_ENCODING).unwrap(),
        "gzip"
    );
    assert_eq!(
        hit.headers().get(http::header::VARY).unwrap(),
        "Accept-Encoding"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn head_does_not_poison_get_and_serves_cached_headers() {
    let calls = Arc::new(AtomicUsize::new(0));
    let layer = CacheLayer::builder(InMemoryBackend::new(100)).build();
    let mut svc = layer.layer(service_fn({
        let calls = calls.clone();
        move |r: Request<()>| {
            calls.fetch_add(1, Ordering::SeqCst);
            async move {
                let body = if r.method() == http::Method::HEAD {
                    ""
                } else {
                    "full body"
                };
                Ok::<_, Infallible>(
                    http::Response::builder()
                        .header("content-length", "9")
                        .body(Full::from(body))
                        .unwrap(),
                )
            }
        }
    }));

    // HEAD miss must not store an empty body into GET's cache slot.
    let head_miss = svc
        .ready()
        .await
        .unwrap()
        .call(Request::head("/h").body(()).unwrap())
        .await
        .unwrap();
    assert_eq!(head_miss.status(), StatusCode::OK);
    assert!(
        head_miss
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );

    // Subsequent GET hits origin and populates cache with the full body.
    let get_miss = svc
        .ready()
        .await
        .unwrap()
        .call(Request::get("/h").body(()).unwrap())
        .await
        .unwrap();
    assert_eq!(get_miss.headers().get("content-length").unwrap(), "9");
    assert_eq!(
        get_miss.into_body().collect().await.unwrap().to_bytes(),
        Bytes::from_static(b"full body")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // Subsequent HEAD hits cache without calling origin, keeping content-length=9 and empty body.
    let head_hit = svc
        .ready()
        .await
        .unwrap()
        .call(Request::head("/h").body(()).unwrap())
        .await
        .unwrap();
    assert_eq!(head_hit.headers().get("content-length").unwrap(), "9");
    assert!(
        head_hit
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn axum_head_preserves_content_length_on_miss_and_hit() {
    use axum::{Router, body::Body, error_handling::HandleErrorLayer, routing::get};

    let expected_body = b"hello from GET handler";
    let expected_len = expected_body.len().to_string();
    let calls = Arc::new(AtomicUsize::new(0));

    let layer = CacheLayer::builder(InMemoryBackend::new(100)).build();
    let app = Router::new()
        .route(
            "/",
            get({
                let calls = calls.clone();
                move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    async move { "hello from GET handler" }
                }
            }),
        )
        .layer(
            tower::ServiceBuilder::new()
                .layer(HandleErrorLayer::new(
                    |_e: tower_http_cache::layer::BoxError| async {
                        StatusCode::INTERNAL_SERVER_ERROR
                    },
                ))
                .layer(layer),
        );

    // 1. HEAD miss must preserve GET body Content-Length and return empty body.
    let head_miss = app
        .clone()
        .oneshot(Request::head("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(head_miss.status(), StatusCode::OK);
    assert_eq!(
        head_miss
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()),
        Some(expected_len.as_str())
    );
    assert!(
        head_miss
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // 2. GET populates the cache and returns the full body.
    let get_resp = app
        .clone()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);
    let body = get_resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], expected_body);
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // 3. HEAD hit must also preserve GET body Content-Length and return empty body.
    let head_hit = app
        .clone()
        .oneshot(Request::head("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(head_hit.status(), StatusCode::OK);
    assert_eq!(
        head_hit
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()),
        Some(expected_len.as_str())
    );
    assert!(
        head_hit
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // 4. Subsequent GET still hits cache and receives the full body.
    let get_hit = app
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        &get_hit.into_body().collect().await.unwrap().to_bytes()[..],
        expected_body
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn axum_head_does_not_poison_get() {
    use axum::{Router, body::Body, error_handling::HandleErrorLayer, routing::get};

    let layer = CacheLayer::builder(InMemoryBackend::new(100)).build();
    let app = Router::new()
        .route("/", get(|| async { "hello from GET handler" }))
        .layer(
            tower::ServiceBuilder::new()
                .layer(HandleErrorLayer::new(
                    |_e: tower_http_cache::layer::BoxError| async {
                        StatusCode::INTERNAL_SERVER_ERROR
                    },
                ))
                .layer(layer),
        );

    let head = app
        .clone()
        .oneshot(Request::head("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);

    let get_resp = app
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = get_resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"hello from GET handler");
}

#[tokio::test]
async fn cookie_and_authorization_follow_rfc9111_shared_cache_rules() {
    let calls = Arc::new(AtomicUsize::new(0));
    let layer = CacheLayer::builder(InMemoryBackend::new(100))
        .ttl(Duration::from_secs(60))
        .build();
    let mut svc = layer.layer(service_fn({
        let calls = calls.clone();
        move |req: Request<()>| {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            let is_public = req.uri().path() == "/public-auth";
            async move {
                let mut builder = http::Response::builder().status(StatusCode::OK);
                if is_public {
                    builder = builder.header(CACHE_CONTROL, "public, max-age=60");
                }
                Ok::<_, Infallible>(builder.body(Full::from(format!("resp-{n}"))).unwrap())
            }
        }
    }));

    // Cookie requests always bypass lookup & store.
    for _ in 0..2 {
        let _ = svc
            .ready()
            .await
            .unwrap()
            .call(
                Request::get("/cookie")
                    .header(http::header::COOKIE, "sid=secret")
                    .body(())
                    .unwrap(),
            )
            .await
            .unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // Public response cached without Authorization is still private-to-Authorization without public directive.
    let r1 = svc
        .ready()
        .await
        .unwrap()
        .call(Request::get("/auth-default").body(()).unwrap())
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let r2 = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/auth-default")
                .header(http::header::AUTHORIZATION, "Bearer user1")
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
    assert_ne!(
        r1, r2,
        "authenticated request must not reuse non-public entry"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);

    // Explicit `Cache-Control: public` allows caching & hitting for Authorization requests.
    let p1 = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/public-auth")
                .header(http::header::AUTHORIZATION, "Bearer user1")
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
    let p2 = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/public-auth")
                .header(http::header::AUTHORIZATION, "Bearer user1")
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
    assert_eq!(p1, p2);
    assert_eq!(calls.load(Ordering::SeqCst), 5);
}

#[cfg(feature = "admin-api")]
#[tokio::test]
async fn global_stats_key_hits_is_bounded() {
    let layer = CacheLayer::builder(InMemoryBackend::new(10)).build();
    let stats = layer.stats();
    let mut svc = layer.layer(service_fn(|_r: Request<()>| async {
        Ok::<_, Infallible>(http::Response::new(Full::from("ok")))
    }));

    for i in 0..1500 {
        let uri = format!("/k?i={i}");
        let _ = svc
            .ready()
            .await
            .unwrap()
            .call(Request::get(uri.as_str()).body(()).unwrap())
            .await
            .unwrap();
        let _ = svc
            .ready()
            .await
            .unwrap()
            .call(Request::get(uri.as_str()).body(()).unwrap())
            .await
            .unwrap();
    }

    let tracked = stats.hot_keys(usize::MAX).len();
    assert!(
        tracked <= 1_000,
        "GlobalStats hot-key tracking must remain bounded (got {tracked})"
    );
}

#[tokio::test]
async fn cookie_policy_supports_bypass_require_public_and_ignore() {
    use tower_http_cache::policy::CookiePolicy;

    // 1. Bypass (default): Cookie bypasses even `Cache-Control: public`.
    let bypass_calls = Arc::new(AtomicUsize::new(0));
    let bypass_layer = CacheLayer::builder(InMemoryBackend::new(64))
        .ttl(Duration::from_secs(60))
        .cookie_policy(CookiePolicy::Bypass)
        .build();
    let mut bypass_svc = bypass_layer.layer(service_fn({
        let calls = bypass_calls.clone();
        move |_req: Request<()>| {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                Ok::<_, Infallible>(
                    http::Response::builder()
                        .header(CACHE_CONTROL, "public, max-age=60")
                        .body(Full::from(format!("bypass-{n}")))
                        .unwrap(),
                )
            }
        }
    }));
    for _ in 0..2 {
        let _ = bypass_svc
            .ready()
            .await
            .unwrap()
            .call(
                Request::get("/item")
                    .header(http::header::COOKIE, "sid=1")
                    .body(())
                    .unwrap(),
            )
            .await
            .unwrap();
    }
    assert_eq!(bypass_calls.load(Ordering::SeqCst), 2);

    // 2. RequirePublic: skips non-public responses, caches `public` and `s-maxage`.
    let pub_calls = Arc::new(AtomicUsize::new(0));
    let pub_layer = CacheLayer::builder(InMemoryBackend::new(64))
        .ttl(Duration::from_secs(60))
        .cookie_policy(CookiePolicy::RequirePublic)
        .build();
    let mut pub_svc = pub_layer.layer(service_fn({
        let calls = pub_calls.clone();
        move |req: Request<()>| {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            let path = req.uri().path().to_owned();
            async move {
                let mut builder = http::Response::builder();
                if path == "/public" {
                    builder = builder.header(CACHE_CONTROL, "public, max-age=60");
                } else if path == "/s-maxage" {
                    builder = builder.header(CACHE_CONTROL, "s-maxage=60");
                }
                Ok::<_, Infallible>(builder.body(Full::from(format!("pub-{n}"))).unwrap())
            }
        }
    }));

    // Non-public endpoint with Cookie must not be stored or served.
    for _ in 0..2 {
        let _ = pub_svc
            .ready()
            .await
            .unwrap()
            .call(
                Request::get("/plain")
                    .header(http::header::COOKIE, "ga=1")
                    .body(())
                    .unwrap(),
            )
            .await
            .unwrap();
    }
    assert_eq!(pub_calls.load(Ordering::SeqCst), 2);

    // `public` and `s-maxage` endpoints cache and hit despite Cookie.
    for path in ["/public", "/s-maxage"] {
        let first = pub_svc
            .ready()
            .await
            .unwrap()
            .call(
                Request::get(path)
                    .header(http::header::COOKIE, "ga=1")
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
        let second = pub_svc
            .ready()
            .await
            .unwrap()
            .call(
                Request::get(path)
                    .header(http::header::COOKIE, "ga=2")
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
        assert_eq!(first, second);
    }
    assert_eq!(pub_calls.load(Ordering::SeqCst), 4);

    // 3. Ignore: caches & hits with Cookie even without `public`, still strips Set-Cookie.
    let ign_calls = Arc::new(AtomicUsize::new(0));
    let ign_layer = CacheLayer::builder(InMemoryBackend::new(64))
        .ttl(Duration::from_secs(60))
        .cookie_policy(CookiePolicy::Ignore)
        .build();
    let mut ign_svc = ign_layer.layer(service_fn({
        let calls = ign_calls.clone();
        move |_req: Request<()>| {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                Ok::<_, Infallible>(
                    http::Response::builder()
                        .header(http::header::SET_COOKIE, "secret=1")
                        .body(Full::from(format!("ign-{n}")))
                        .unwrap(),
                )
            }
        }
    }));
    let r1 = ign_svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/ign")
                .header(http::header::COOKIE, "a=1")
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
    let r2 = ign_svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/ign")
                .header(http::header::COOKIE, "b=2")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(r2.headers().get(http::header::SET_COOKIE).is_none());
    let b2 = r2.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(r1, b2);
    assert_eq!(ign_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn upstream_content_encoding_negotiated_on_full_and_chunk_hits() {
    use tower_http_cache::streaming::StreamingPolicy;

    // Part 1: Full cache hit negotiates upstream Content-Encoding against Accept-Encoding.
    let full_calls = Arc::new(AtomicUsize::new(0));
    let full_layer = CacheLayer::builder(InMemoryBackend::new(64))
        .ttl(Duration::from_secs(60))
        .header_allowlist(["content-type"])
        .build();
    let mut full_svc = full_layer.layer(service_fn({
        let calls = full_calls.clone();
        move |req: Request<()>| {
            calls.fetch_add(1, Ordering::SeqCst);
            let wants_gzip = req
                .headers()
                .get(http::header::ACCEPT_ENCODING)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("gzip"));
            async move {
                let mut builder = http::Response::builder().header("content-type", "text/plain");
                if wants_gzip {
                    builder = builder
                        .header(http::header::CONTENT_ENCODING, "gzip")
                        .header(http::header::VARY, "Accept-Encoding");
                    Ok::<_, Infallible>(
                        builder
                            .body(Full::from(Bytes::from_static(b"\x1f\x8b\x08\x00")))
                            .unwrap(),
                    )
                } else {
                    Ok::<_, Infallible>(
                        builder
                            .body(Full::from(Bytes::from_static(b"plain-text")))
                            .unwrap(),
                    )
                }
            }
        }
    }));

    // Seed cache with gzip-encoded upstream response.
    let gz_resp = full_svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/enc")
                .header(http::header::ACCEPT_ENCODING, "gzip")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        gz_resp
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .unwrap(),
        "gzip"
    );

    // Matching Accept-Encoding hits cache.
    let gz_hit = full_svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/enc")
                .header(http::header::ACCEPT_ENCODING, "br, gzip")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        gz_hit
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .unwrap(),
        "gzip"
    );
    assert_eq!(full_calls.load(Ordering::SeqCst), 1);

    // Client without Accept-Encoding must NOT receive upstream gzip bytes.
    let plain_resp = full_svc
        .ready()
        .await
        .unwrap()
        .call(Request::get("/enc").body(()).unwrap())
        .await
        .unwrap();
    assert!(
        plain_resp
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .is_none()
    );
    let plain_body = plain_resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&plain_body[..], b"plain-text");
    assert_eq!(full_calls.load(Ordering::SeqCst), 2);

    // Part 2: Chunk cache preserves Content-Encoding/Vary under allowlist and negotiates Accept-Encoding.
    let chunk_calls = Arc::new(AtomicUsize::new(0));
    let chunk_layer = CacheLayer::builder(InMemoryBackend::new(64))
        .policy(
            tower_http_cache::policy::CachePolicy::default()
                .with_ttl(Duration::from_secs(60))
                .with_header_allowlist(["content-type"])
                .with_streaming_policy(StreamingPolicy {
                    enable_chunk_cache: true,
                    chunk_size: 8,
                    min_chunk_file_size: 4,
                    excluded_content_types: std::collections::HashSet::new(),
                    ..Default::default()
                }),
        )
        .build();
    let mut chunk_svc = chunk_layer.layer(service_fn({
        let calls = chunk_calls.clone();
        move |req: Request<()>| {
            calls.fetch_add(1, Ordering::SeqCst);
            let wants_br = req
                .headers()
                .get(http::header::ACCEPT_ENCODING)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("br"));
            async move {
                let mut builder =
                    http::Response::builder().header("content-type", "application/octet-stream");
                if wants_br {
                    builder = builder
                        .header(http::header::CONTENT_ENCODING, "br")
                        .header(http::header::VARY, "Accept-Encoding");
                    Ok::<_, Infallible>(
                        builder
                            .body(Full::from(Bytes::from_static(b"0123456789abcdef")))
                            .unwrap(),
                    )
                } else {
                    Ok::<_, Infallible>(
                        builder
                            .body(Full::from(Bytes::from_static(b"plain-chunk-body")))
                            .unwrap(),
                    )
                }
            }
        }
    }));

    // Populate chunk cache with `br` upstream response.
    let _ = chunk_svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/blob")
                .header(http::header::ACCEPT_ENCODING, "br")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap();
    assert_eq!(chunk_calls.load(Ordering::SeqCst), 1);

    // Range request with `Accept-Encoding: br` hits chunk cache and preserves Content-Encoding + Vary.
    let chunk_hit = chunk_svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/blob")
                .header(http::header::RANGE, "bytes=0-3")
                .header(http::header::ACCEPT_ENCODING, "br")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(chunk_hit.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        chunk_hit
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .unwrap(),
        "br"
    );
    assert_eq!(
        chunk_hit.headers().get(http::header::VARY).unwrap(),
        "Accept-Encoding"
    );
    assert_eq!(chunk_calls.load(Ordering::SeqCst), 1);

    // Range request without `br` must NOT hit the `br`-encoded chunk entry.
    let mismatch = chunk_svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/blob")
                .header(http::header::RANGE, "bytes=0-3")
                .header(http::header::ACCEPT_ENCODING, "gzip")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        mismatch
            .headers()
            .get(http::header::CONTENT_ENCODING)
            .is_none()
    );
    let mismatch_body = mismatch.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&mismatch_body[..], b"plain-chunk-body");
    assert_eq!(chunk_calls.load(Ordering::SeqCst), 2);
}

#[cfg(all(feature = "serde", feature = "tracing"))]
#[tokio::test]
async fn ml_logging_bypass_emits_single_bypass_event_and_hashes_uri() {
    use std::io::Write;
    use std::sync::Mutex;
    use tower_http_cache::logging::MLLoggingConfig;
    use tower_http_cache::policy::CachePolicy;

    #[derive(Clone, Default)]
    struct BufWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for BufWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufWriter {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let logs = Arc::new(Mutex::new(Vec::<u8>::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(BufWriter(logs.clone()))
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let policy = CachePolicy::default().with_ml_logging(
        MLLoggingConfig::new()
            .with_enabled(true)
            .with_sample_rate(1.0)
            .with_hash_keys(true),
    );
    let layer = CacheLayer::builder(InMemoryBackend::new(64))
        .policy(policy)
        .build();
    let mut svc = layer.layer(service_fn(|_req: Request<()>| async {
        Ok::<_, Infallible>(http::Response::new(Full::from("ok")))
    }));

    let _ = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::post("/secret-bypass-path?token=xyz")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();

    let output = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    let bypass_count = output.matches("\"event\":\"bypass\"").count();
    let miss_count = output.matches("\"event\":\"miss\"").count();
    assert_eq!(bypass_count, 1, "expected exactly 1 bypass event: {output}");
    assert_eq!(
        miss_count, 0,
        "bypassed request must not emit miss: {output}"
    );
    assert!(
        !output.contains("secret-bypass-path") && !output.contains("token=xyz"),
        "hash_keys=true must hash both key and uri: {output}"
    );
}

#[tokio::test]
async fn vary_cookie_authorization_and_star_prevent_cross_user_sharing() {
    use tower_http_cache::policy::{CachePolicy, CookiePolicy};
    use tower_http_cache::streaming::StreamingPolicy;

    for policy_kind in [CookiePolicy::RequirePublic, CookiePolicy::Ignore] {
        for vary_value in ["Cookie", "*", "Accept-Encoding, cookie"] {
            let calls = Arc::new(AtomicUsize::new(0));
            let layer = CacheLayer::builder(InMemoryBackend::new(64))
                .ttl(Duration::from_secs(60))
                .cookie_policy(policy_kind)
                .build();
            let mut svc = layer.layer(service_fn({
                let calls = calls.clone();
                let vary_value = vary_value.to_string();
                move |req: Request<()>| {
                    let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                    let cookie = req
                        .headers()
                        .get(http::header::COOKIE)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("none")
                        .to_owned();
                    let vary_header = vary_value.clone();
                    async move {
                        Ok::<_, Infallible>(
                            http::Response::builder()
                                .header(CACHE_CONTROL, "public, max-age=60")
                                .header(http::header::VARY, vary_header)
                                .body(Full::from(format!("hello {cookie} #{n}")))
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
                    Request::get("/profile")
                        .header(http::header::COOKIE, "sid=alice")
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
                    Request::get("/profile")
                        .header(http::header::COOKIE, "sid=bob")
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

            assert_eq!(&alice[..], b"hello sid=alice #1");
            assert_eq!(
                &bob[..],
                b"hello sid=bob #2",
                "Vary: {vary_value} under {policy_kind:?} must not serve alice's entry to bob"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 2);
        }

        // Unrelated Vary (e.g. Vary: Origin) still caches under RequirePublic/Ignore.
        let origin_calls = Arc::new(AtomicUsize::new(0));
        let origin_layer = CacheLayer::builder(InMemoryBackend::new(64))
            .ttl(Duration::from_secs(60))
            .cookie_policy(policy_kind)
            .build();
        let mut origin_svc = origin_layer.layer(service_fn({
            let calls = origin_calls.clone();
            move |_req: Request<()>| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    Ok::<_, Infallible>(
                        http::Response::builder()
                            .header(CACHE_CONTROL, "public, max-age=60")
                            .header(http::header::VARY, "Origin")
                            .body(Full::from(format!("cors-{n}")))
                            .unwrap(),
                    )
                }
            }
        }));
        let r1 = origin_svc
            .ready()
            .await
            .unwrap()
            .call(
                Request::get("/cors")
                    .header(http::header::COOKIE, "sid=alice")
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
        let r2 = origin_svc
            .ready()
            .await
            .unwrap()
            .call(
                Request::get("/cors")
                    .header(http::header::COOKIE, "sid=bob")
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
        assert_eq!(r1, r2);
        assert_eq!(origin_calls.load(Ordering::SeqCst), 1);
    }

    // Anonymous store with header_allowlist omitting `vary` must still block a Cookie hit
    // when the stored entry declared `Vary: Cookie` (both full hit and chunk-cache hit).
    let anon_calls = Arc::new(AtomicUsize::new(0));
    let anon_layer = CacheLayer::builder(InMemoryBackend::new(64))
        .policy(
            CachePolicy::default()
                .with_ttl(Duration::from_secs(60))
                .with_cookie_policy(CookiePolicy::RequirePublic)
                .with_header_allowlist(["content-type", "cache-control"])
                .with_streaming_policy(StreamingPolicy {
                    enable_chunk_cache: true,
                    chunk_size: 4,
                    min_chunk_file_size: 4,
                    excluded_content_types: std::collections::HashSet::new(),
                    ..Default::default()
                }),
        )
        .build();
    let mut anon_svc = anon_layer.layer(service_fn({
        let calls = anon_calls.clone();
        move |req: Request<()>| {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            let has_cookie = req.headers().contains_key(http::header::COOKIE);
            async move {
                let body = if has_cookie {
                    format!("user-{n}")
                } else {
                    "anon-01234567".to_string()
                };
                Ok::<_, Infallible>(
                    http::Response::builder()
                        .header("content-type", "text/plain")
                        .header(CACHE_CONTROL, "public, max-age=60")
                        .header(http::header::VARY, "Cookie")
                        .body(Full::from(body))
                        .unwrap(),
                )
            }
        }
    }));

    // Anonymous request seeds both main cache and chunk cache.
    let anon = anon_svc
        .ready()
        .await
        .unwrap()
        .call(Request::get("/shared").body(()).unwrap())
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(&anon[..], b"anon-01234567");

    // Cookie range request must NOT hit chunk cache or main cache.
    let cookie_range = anon_svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/shared")
                .header(http::header::COOKIE, "sid=alice")
                .header(http::header::RANGE, "bytes=0-3")
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
    assert_eq!(&cookie_range[..], b"user-2");

    // Authorization + Vary: Authorization / Vary: * must not store or share.
    for vary_value in ["Authorization", "*"] {
        let auth_calls = Arc::new(AtomicUsize::new(0));
        let auth_layer = CacheLayer::builder(InMemoryBackend::new(64))
            .ttl(Duration::from_secs(60))
            .build();
        let mut auth_svc = auth_layer.layer(service_fn({
            let calls = auth_calls.clone();
            let vary_value = vary_value.to_string();
            move |req: Request<()>| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                let auth = req
                    .headers()
                    .get(http::header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("none")
                    .to_owned();
                let vary_header = vary_value.clone();
                async move {
                    Ok::<_, Infallible>(
                        http::Response::builder()
                            .header(CACHE_CONTROL, "public, max-age=60")
                            .header(http::header::VARY, vary_header)
                            .body(Full::from(format!("{auth} #{n}")))
                            .unwrap(),
                    )
                }
            }
        }));
        let u1 = auth_svc
            .ready()
            .await
            .unwrap()
            .call(
                Request::get("/me")
                    .header(http::header::AUTHORIZATION, "Bearer u1")
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
        let u2 = auth_svc
            .ready()
            .await
            .unwrap()
            .call(
                Request::get("/me")
                    .header(http::header::AUTHORIZATION, "Bearer u2")
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
        assert_eq!(&u1[..], b"Bearer u1 #1");
        assert_eq!(&u2[..], b"Bearer u2 #2");
        assert_eq!(auth_calls.load(Ordering::SeqCst), 2);
    }
}
