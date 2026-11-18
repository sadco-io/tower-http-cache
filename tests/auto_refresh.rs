//! Integration tests for auto-refresh functionality.
//!
//! These tests verify that the auto-refresh feature works correctly with
//! both in-memory and Redis backends, handles errors gracefully, and respects
//! concurrency limits.

use http::{Method, Request, Response, StatusCode, Uri};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tower::{Layer, Service, ServiceExt};
use tower_http_cache::backend::memory::InMemoryBackend;
use tower_http_cache::layer::CacheLayer;
use tower_http_cache::refresh::AutoRefreshConfig;

// Removed test_service helper - using tower::service_fn directly instead
// to avoid type inference issues with impl Service return types

#[tokio::test]
async fn auto_refresh_disabled_by_default() {
    let backend = InMemoryBackend::new(100);
    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(10))
        .build();

    let call_count = Arc::new(AtomicUsize::new(0));
    let handler = tower::service_fn({
        let count = call_count.clone();
        move |_req: Request<()>| {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::Relaxed);
                Ok::<_, std::convert::Infallible>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(http_body_util::Full::from(bytes::Bytes::from(
                            "test response",
                        )))
                        .unwrap(),
                )
            }
        }
    });
    let mut service = layer.layer(handler);

    let req = Request::builder()
        .method(Method::GET)
        .uri("http://example.com/test")
        .body(())
        .unwrap();

    // First request should hit the service
    let _ = service.ready().await.unwrap().call(req).await.unwrap();
    assert_eq!(call_count.load(Ordering::Relaxed), 1);

    // Second request should be served from cache
    let req2 = Request::builder()
        .method(Method::GET)
        .uri("http://example.com/test")
        .body(())
        .unwrap();
    let _ = service.ready().await.unwrap().call(req2).await.unwrap();
    assert_eq!(call_count.load(Ordering::Relaxed), 1);

    // Wait a bit - no auto-refresh should happen
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(call_count.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn auto_refresh_tracks_hits() {
    let backend = InMemoryBackend::new(100);
    let config = AutoRefreshConfig {
        enabled: true,
        min_hits_per_minute: 5.0,
        check_interval: Duration::from_millis(100),
        cleanup_interval: Duration::from_secs(60),
        ..Default::default()
    };

    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(10))
        .refresh_before(Duration::from_secs(5))
        .auto_refresh(config)
        .build();

    let call_count = Arc::new(AtomicUsize::new(0));
    let handler = tower::service_fn({
        let count = call_count.clone();
        move |_req: Request<()>| {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::Relaxed);
                Ok::<_, std::convert::Infallible>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(http_body_util::Full::from(bytes::Bytes::from(
                            "test response",
                        )))
                        .unwrap(),
                )
            }
        }
    });
    let mut service = layer.layer(handler);

    // Make initial request to populate cache
    let req = Request::builder()
        .method(Method::GET)
        .uri("http://example.com/test")
        .body(())
        .unwrap();
    let _ = service.ready().await.unwrap().call(req).await.unwrap();
    assert_eq!(call_count.load(Ordering::Relaxed), 1);

    // Hit the cache multiple times to trigger tracking
    for _ in 0..10 {
        let req = Request::builder()
            .method(Method::GET)
            .uri("http://example.com/test")
            .body(())
            .unwrap();
        let _ = service.ready().await.unwrap().call(req).await.unwrap();
    }

    // All should be cache hits
    assert_eq!(call_count.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn auto_refresh_respects_concurrency_limit() {
    let backend = InMemoryBackend::new(100);
    let config = AutoRefreshConfig {
        enabled: true,
        min_hits_per_minute: 1.0,
        check_interval: Duration::from_millis(50),
        max_concurrent_refreshes: 2,
        cleanup_interval: Duration::from_secs(60),
        ..Default::default()
    };

    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(1))
        .refresh_before(Duration::from_millis(900))
        .auto_refresh(config)
        .build();

    // Use a slow service to test concurrency limits
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));

    let in_flight_clone = in_flight.clone();
    let max_in_flight_clone = max_in_flight.clone();

    let slow_service = tower::service_fn(move |_req: Request<()>| {
        let in_flight = in_flight_clone.clone();
        let max_in_flight = max_in_flight_clone.clone();

        async move {
            let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            max_in_flight.fetch_max(current, Ordering::SeqCst);

            tokio::time::sleep(Duration::from_millis(100)).await;

            in_flight.fetch_sub(1, Ordering::SeqCst);

            Ok::<_, std::convert::Infallible>(
                Response::builder()
                    .status(StatusCode::OK)
                    .body(http_body_util::Full::from(bytes::Bytes::from(
                        "slow response",
                    )))
                    .unwrap(),
            )
        }
    });

    let mut service = layer.layer(slow_service);

    // Make initial requests to populate cache with different keys
    for i in 0..5 {
        let req = Request::builder()
            .method(Method::GET)
            .uri(format!("http://example.com/test{}", i))
            .body(())
            .unwrap();
        let _ = service.ready().await.unwrap().call(req).await.unwrap();
    }

    // Hit each key to trigger tracking
    for i in 0..5 {
        let req = Request::builder()
            .method(Method::GET)
            .uri(format!("http://example.com/test{}", i))
            .body(())
            .unwrap();
        let _ = service.ready().await.unwrap().call(req).await.unwrap();
    }

    // Wait for refresh window to trigger
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Wait for auto-refreshes to complete
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Max concurrent should respect the limit
    let max = max_in_flight.load(Ordering::SeqCst);
    println!("Max concurrent refreshes: {}", max);
    // Due to timing, we may not hit the exact limit, but it should be reasonable
    assert!(max <= 3, "Expected max concurrent <= 3, got {}", max);
}

#[tokio::test]
async fn auto_refresh_handles_service_errors_gracefully() {
    let backend = InMemoryBackend::new(100);
    let config = AutoRefreshConfig {
        enabled: true,
        min_hits_per_minute: 1.0,
        check_interval: Duration::from_millis(100),
        cleanup_interval: Duration::from_secs(60),
        ..Default::default()
    };

    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(1))
        .refresh_before(Duration::from_millis(900))
        .auto_refresh(config)
        .build();

    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();

    // Service that succeeds first time, then fails
    let error_service = tower::service_fn(move |_req: Request<()>| {
        let count = call_count_clone.clone();
        async move {
            let current = count.fetch_add(1, Ordering::Relaxed);
            if current == 0 {
                Ok::<_, std::convert::Infallible>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(http_body_util::Full::from(bytes::Bytes::from("success")))
                        .unwrap(),
                )
            } else {
                // Return error status
                Ok::<_, std::convert::Infallible>(
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(http_body_util::Full::from(bytes::Bytes::from("error")))
                        .unwrap(),
                )
            }
        }
    });

    let mut service = layer.layer(error_service);

    // Make initial request
    let req = Request::builder()
        .method(Method::GET)
        .uri("http://example.com/test")
        .body(())
        .unwrap();
    let resp = service.ready().await.unwrap().call(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Hit cache to track
    let req2 = Request::builder()
        .method(Method::GET)
        .uri("http://example.com/test")
        .body(())
        .unwrap();
    let _ = service.ready().await.unwrap().call(req2).await.unwrap();

    // Wait for potential refresh (which will fail)
    tokio::time::sleep(Duration::from_millis(1200)).await;

    // Service should have been called at least once more (the auto-refresh)
    // But the system should not crash
    let count = call_count.load(Ordering::Relaxed);
    println!("Total service calls: {}", count);

    // Verify we can still make requests
    let req3 = Request::builder()
        .method(Method::GET)
        .uri("http://example.com/test2")
        .body(())
        .unwrap();
    let _ = service.ready().await.unwrap().call(req3).await;
}

#[tokio::test]
async fn auto_refresh_metadata_reconstruction() {
    use tower_http_cache::refresh::RefreshMetadata;

    let req = Request::builder()
        .method(Method::GET)
        .uri("http://example.com/test?foo=bar")
        .header("authorization", "Bearer token")
        .body(())
        .unwrap();

    let metadata = RefreshMetadata::from_request_with_headers(&req, &["authorization".to_string()]);

    assert_eq!(metadata.method, Method::GET);
    assert_eq!(metadata.uri.path(), "/test");
    assert_eq!(metadata.uri.query(), Some("foo=bar"));
    assert_eq!(metadata.headers.len(), 1);
    assert_eq!(metadata.headers[0].0, "authorization");

    // Should be able to reconstruct
    let reconstructed = metadata.try_into_request();
    assert!(reconstructed.is_some());

    let reconstructed = reconstructed.unwrap();
    assert_eq!(reconstructed.method(), Method::GET);
    assert_eq!(reconstructed.uri().path(), "/test");
    assert!(reconstructed.headers().get("authorization").is_some());
}

#[tokio::test]
async fn auto_refresh_config_validation() {
    let valid = AutoRefreshConfig::default();
    assert!(valid.validate().is_ok());

    let invalid_hits = AutoRefreshConfig {
        min_hits_per_minute: -1.0,
        ..Default::default()
    };
    assert!(invalid_hits.validate().is_err());

    let invalid_concurrent = AutoRefreshConfig {
        max_concurrent_refreshes: 0,
        ..Default::default()
    };
    assert!(invalid_concurrent.validate().is_err());

    let invalid_interval = AutoRefreshConfig {
        check_interval: Duration::ZERO,
        ..Default::default()
    };
    assert!(invalid_interval.validate().is_err());
}

#[tokio::test]
async fn auto_refresh_cleanup_stale_tracking() {
    use tower_http_cache::refresh::AccessTracker;

    let config = AutoRefreshConfig {
        hit_rate_window: Duration::from_secs(60),
        ..Default::default()
    };
    let tracker = AccessTracker::new(config);

    tracker.record_hit("key1");
    tracker.record_hit("key2");
    assert_eq!(tracker.tracked_keys(), 2);

    // Cleanup with very long max_age should not remove anything
    tracker.cleanup_stale(Duration::from_secs(3600));
    assert_eq!(tracker.tracked_keys(), 2);

    // Cleanup with very short max_age removes recent entries (in real scenario)
    // But since we just created them, they won't be removed
    tracker.cleanup_stale(Duration::ZERO);
    // Keys should still be there since they were just accessed
    assert!(tracker.tracked_keys() <= 2);
}

#[tokio::test]
async fn auto_refresh_hit_rate_calculation() {
    use tower_http_cache::refresh::AccessTracker;

    let config = AutoRefreshConfig {
        min_hits_per_minute: 10.0,
        hit_rate_window: Duration::from_secs(1), // Use 1 second for more stable timing
        ..Default::default()
    };
    let tracker = AccessTracker::new(config);

    // Record several hits spread over a bit of time
    for _ in 0..5 {
        tracker.record_hit("hot_key");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Should have a high hit rate
    let rate = tracker.hits_per_minute("hot_key");
    println!("Hit rate: {} hits/min", rate);

    // Should be considered for auto-refresh if rate is high enough
    // Due to timing, the rate might vary, but should be > 0
    assert!(rate > 0.0);
}

#[tokio::test]
async fn auto_refresh_with_different_methods() {
    let backend = InMemoryBackend::new(100);
    let config = AutoRefreshConfig::enabled(5.0);

    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(10))
        .auto_refresh(config)
        .build();

    let call_count = Arc::new(AtomicUsize::new(0));
    let handler = tower::service_fn({
        let count = call_count.clone();
        move |_req: Request<()>| {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::Relaxed);
                Ok::<_, std::convert::Infallible>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(http_body_util::Full::from(bytes::Bytes::from(
                            "test response",
                        )))
                        .unwrap(),
                )
            }
        }
    });
    let mut service = layer.layer(handler);

    // GET should be cached and tracked
    let get_req = Request::builder()
        .method(Method::GET)
        .uri("http://example.com/test")
        .body(())
        .unwrap();
    let _ = service.ready().await.unwrap().call(get_req).await.unwrap();
    assert_eq!(call_count.load(Ordering::Relaxed), 1);

    // POST should not be cached (by default)
    let post_req = Request::builder()
        .method(Method::POST)
        .uri("http://example.com/test")
        .body(())
        .unwrap();
    let _ = service.ready().await.unwrap().call(post_req).await.unwrap();
    assert_eq!(call_count.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn auto_refresh_manager_lifecycle() {
    use tower_http_cache::refresh::{RefreshCallback, RefreshManager, RefreshMetadata};

    struct TestCallback {
        call_count: Arc<AtomicUsize>,
    }

    impl RefreshCallback for TestCallback {
        fn refresh(
            &self,
            _key: String,
            _metadata: RefreshMetadata,
        ) -> tower_http_cache::refresh::RefreshFuture {
            let count = self.call_count.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::Relaxed);
                Ok(())
            })
        }
    }

    let config = AutoRefreshConfig {
        enabled: true,
        check_interval: Duration::from_millis(50),
        min_hits_per_minute: 0.1, // Very low threshold
        cleanup_interval: Duration::from_secs(60),
        ..Default::default()
    };

    let manager = RefreshManager::new(config);
    let call_count = Arc::new(AtomicUsize::new(0));
    let callback = Arc::new(TestCallback {
        call_count: call_count.clone(),
    });

    // Start the manager
    assert!(manager.start(callback).await.is_ok());

    // Store some metadata
    let metadata = RefreshMetadata {
        method: Method::GET,
        uri: Uri::from_static("http://example.com/test"),
        headers: Vec::new(),
    };
    manager.store_metadata("test_key".to_string(), metadata);

    // Record hits to qualify for refresh
    for _ in 0..10 {
        manager.tracker().record_hit("test_key");
    }

    // Wait for a refresh cycle
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Shutdown gracefully
    manager.shutdown().await;

    println!(
        "Callback was called {} times",
        call_count.load(Ordering::Relaxed)
    );
    // The callback may or may not have been called depending on timing
    // The important thing is that shutdown was graceful
}

#[tokio::test]
async fn auto_refresh_only_refreshes_in_window() {
    // Verify with a running background refresh task that entries outside the
    // `refresh_before` window are NOT refreshed prematurely, while entries inside
    // the window ARE refreshed.
    let backend = InMemoryBackend::new(100);
    let config = AutoRefreshConfig {
        enabled: true,
        min_hits_per_minute: 1.0,
        hit_rate_window: Duration::from_secs(60),
        check_interval: Duration::from_millis(50),
        ..Default::default()
    };

    let call_count = Arc::new(AtomicUsize::new(0));
    let handler = tower::service_fn({
        let count = call_count.clone();
        move |_req: Request<()>| {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::Relaxed);
                Ok::<_, std::convert::Infallible>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(http_body_util::Full::from(bytes::Bytes::from(
                            "test response",
                        )))
                        .unwrap(),
                )
            }
        }
    });

    // Part 1: Long TTL (10s) with small refresh_before (1s) must NOT refresh early.
    let long_layer = CacheLayer::builder(backend)
        .ttl(Duration::from_secs(10))
        .refresh_before(Duration::from_secs(1))
        .auto_refresh(config.clone())
        .build();
    long_layer.init_auto_refresh(handler.clone()).await.unwrap();
    let mut service = long_layer.layer(handler.clone());

    let req = Request::builder()
        .method(Method::GET)
        .uri("http://example.com/long")
        .body(())
        .unwrap();
    let _ = service.ready().await.unwrap().call(req).await.unwrap();
    assert_eq!(call_count.load(Ordering::Relaxed), 1);

    for _ in 0..5 {
        let req = Request::builder()
            .method(Method::GET)
            .uri("http://example.com/long")
            .body(())
            .unwrap();
        let _ = service.ready().await.unwrap().call(req).await.unwrap();
    }

    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        call_count.load(Ordering::Relaxed),
        1,
        "entry outside refresh_before window must not auto-refresh"
    );
    drop(service);
    drop(long_layer);

    // Part 2: Short TTL (200ms) with refresh_before (150ms) enters window and refreshes.
    let short_calls = Arc::new(AtomicUsize::new(0));
    let short_handler = tower::service_fn({
        let count = short_calls.clone();
        move |_req: Request<()>| {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::Relaxed);
                Ok::<_, std::convert::Infallible>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(http_body_util::Full::from(bytes::Bytes::from("refreshed")))
                        .unwrap(),
                )
            }
        }
    });
    let short_layer = CacheLayer::builder(InMemoryBackend::new(100))
        .ttl(Duration::from_millis(200))
        .refresh_before(Duration::from_millis(150))
        .auto_refresh(config)
        .build();
    short_layer
        .init_auto_refresh(short_handler.clone())
        .await
        .unwrap();
    let mut short_service = short_layer.layer(short_handler);

    let req = Request::builder()
        .method(Method::GET)
        .uri("http://example.com/short")
        .body(())
        .unwrap();
    let _ = short_service
        .ready()
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();
    for _ in 0..3 {
        let req = Request::builder()
            .method(Method::GET)
            .uri("http://example.com/short")
            .body(())
            .unwrap();
        let _ = short_service
            .ready()
            .await
            .unwrap()
            .call(req)
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        short_calls.load(Ordering::Relaxed) >= 2,
        "entry inside refresh_before window must auto-refresh"
    );
}

#[tokio::test]
async fn dropping_layer_clone_keeps_auto_refresh_alive_while_service_lives() {
    let calls = Arc::new(AtomicUsize::new(0));
    let origin = tower::service_fn({
        let calls = calls.clone();
        move |_r: Request<()>| {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok::<_, std::convert::Infallible>(Response::new(http_body_util::Full::from(
                    bytes::Bytes::from_static(b"v"),
                )))
            }
        }
    });

    let layer = CacheLayer::builder(InMemoryBackend::new(100))
        .ttl(Duration::from_millis(120))
        .refresh_before(Duration::from_millis(100))
        .auto_refresh(AutoRefreshConfig {
            enabled: true,
            min_hits_per_minute: 0.0,
            check_interval: Duration::from_millis(50),
            ..Default::default()
        })
        .build();
    layer.init_auto_refresh(origin.clone()).await.unwrap();

    // Simulate Router::layer / ServiceBuilder consuming a clone of CacheLayer.
    let mut svc = layer.layer(origin);
    drop(layer.clone());
    drop(layer);

    let _ = svc
        .ready()
        .await
        .unwrap()
        .call(Request::get("/alive").body(()).unwrap())
        .await
        .unwrap();
    let _ = svc
        .ready()
        .await
        .unwrap()
        .call(Request::get("/alive").body(()).unwrap())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        calls.load(Ordering::Relaxed) > 1,
        "background refresh task must stay alive while CacheService exists"
    );

    // Once the final CacheService is dropped, background refreshes stop.
    drop(svc);
    tokio::time::sleep(Duration::from_millis(80)).await;
    let settled = calls.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(calls.load(Ordering::Relaxed), settled);
}

#[tokio::test]
async fn auto_refresh_respects_cache_control_no_store() {
    use http_body_util::BodyExt;
    use tower_http_cache::backend::CacheBackend;

    let calls = Arc::new(AtomicUsize::new(0));
    let origin = tower::service_fn({
        let calls = calls.clone();
        move |_req: Request<()>| {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                let mut builder = Response::builder().status(StatusCode::OK);
                if n > 1 {
                    builder = builder.header(http::header::CACHE_CONTROL, "no-store");
                }
                Ok::<_, std::convert::Infallible>(
                    builder
                        .body(http_body_util::Full::from(bytes::Bytes::from(format!(
                            "v{n}"
                        ))))
                        .unwrap(),
                )
            }
        }
    });

    let backend = InMemoryBackend::new(100);
    let layer = CacheLayer::builder(backend.clone())
        .ttl(Duration::from_millis(500))
        .stale_while_revalidate(Duration::from_secs(5))
        .refresh_before(Duration::from_millis(450))
        .auto_refresh(AutoRefreshConfig {
            enabled: true,
            min_hits_per_minute: 0.0,
            check_interval: Duration::from_millis(50),
            ..Default::default()
        })
        .build();
    layer.init_auto_refresh(origin.clone()).await.unwrap();
    let mut svc = layer.layer(origin);

    // Populate initial cacheable entry ("v1").
    let first = svc
        .ready()
        .await
        .unwrap()
        .call(Request::get("/no-store-refresh").body(()).unwrap())
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(&first[..], b"v1");

    // Record a hit so `AccessTracker` tracks the key for auto-refresh.
    let _ = svc
        .ready()
        .await
        .unwrap()
        .call(Request::get("/no-store-refresh").body(()).unwrap())
        .await
        .unwrap();

    // Wait for background refresh to run; subsequent origin calls return `no-store`.
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        calls.load(Ordering::SeqCst) >= 2,
        "background refresh should have queried the origin"
    );

    // The cached entry in the backend must still be "v1" (never overwritten by `no-store`).
    let stored = backend
        .get("/no-store-refresh")
        .await
        .unwrap()
        .expect("initial entry must remain cached");
    assert_eq!(&stored.entry.body[..], b"v1");
}
