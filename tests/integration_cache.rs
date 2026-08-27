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
    assert_eq!(first_text, payload);

    service.ready().await.expect("service ready");
    let cached = service
        .call(Request::new(()))
        .await
        .expect("cached call succeeds")
        .into_body()
        .collect()
        .await
        .expect("cached body collected")
        .to_bytes();
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
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}
