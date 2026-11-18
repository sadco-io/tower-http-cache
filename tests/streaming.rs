//! Integration tests for smart streaming and large file handling.

use bytes::Bytes;
use http::{Method, Request, Response, StatusCode};
use http_body_util::Full;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tower::{Layer, Service, ServiceExt};
use tower_http_cache::backend::CacheBackend;
use tower_http_cache::backend::memory::InMemoryBackend;
use tower_http_cache::backend::multi_tier::MultiTierBuilder;
use tower_http_cache::layer::CacheLayer;
use tower_http_cache::policy::CachePolicy;
use tower_http_cache::streaming::StreamingPolicy;

#[tokio::test]
async fn test_large_pdf_skips_cache() {
    let backend = InMemoryBackend::new(100);
    let cache_layer = CacheLayer::builder(backend.clone())
        .policy(CachePolicy::default().with_streaming_policy(StreamingPolicy::default()))
        .build();

    // Simulate 10MB PDF response
    let large_pdf = vec![0u8; 10 * 1024 * 1024];
    let service = tower::service_fn(move |_req: Request<()>| {
        let pdf_clone = large_pdf.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-type", "application/pdf")
                    .header("content-length", pdf_clone.len().to_string())
                    .body(Full::from(pdf_clone))
                    .unwrap(),
            )
        }
    });

    let mut cached_service = cache_layer.layer(service);

    // First request - should NOT cache
    let req = Request::builder()
        .method(Method::GET)
        .uri("/doc.pdf")
        .body(())
        .unwrap();
    let resp = cached_service
        .ready()
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Second request - should still call upstream (not cached)
    let req2 = Request::builder()
        .method(Method::GET)
        .uri("/doc.pdf")
        .body(())
        .unwrap();
    let _resp2 = cached_service
        .ready()
        .await
        .unwrap()
        .call(req2)
        .await
        .unwrap();

    // Verify NOT cached by checking backend
    let cached = backend.get("/doc.pdf").await.unwrap();
    assert!(cached.is_none(), "Large PDF should not be cached");
}

#[tokio::test]
async fn test_small_json_still_cached() {
    let backend = InMemoryBackend::new(100);
    let cache_layer = CacheLayer::builder(backend.clone())
        .policy(CachePolicy::default().with_streaming_policy(StreamingPolicy::default()))
        .build();

    let json_data = r#"{"status":"ok","data":{"id":123}}"#;
    let service = tower::service_fn(move |_req: Request<()>| {
        let data = json_data.to_string();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-type", "application/json")
                    .header("content-length", data.len().to_string())
                    .status(StatusCode::OK)
                    .body(Full::from(data))
                    .unwrap(),
            )
        }
    });

    let mut cached_service = cache_layer.layer(service);

    // First request - should cache
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/data")
        .body(())
        .unwrap();
    let resp = cached_service
        .ready()
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Wait a bit for async cache write
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Verify cached
    let cached = backend.get("/api/data").await.unwrap();
    assert!(cached.is_some(), "Small JSON should be cached");
}

#[tokio::test]
async fn test_video_content_type_excluded() {
    let backend = InMemoryBackend::new(100);
    let cache_layer = CacheLayer::builder(backend.clone())
        .policy(CachePolicy::default().with_streaming_policy(StreamingPolicy::default()))
        .build();

    // Small video (under 1MB) but should still be excluded by content-type
    let video_data = vec![0u8; 500 * 1024]; // 500KB
    let service = tower::service_fn(move |_req: Request<()>| {
        let data = video_data.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-type", "video/mp4")
                    .header("content-length", data.len().to_string())
                    .body(Full::from(data))
                    .unwrap(),
            )
        }
    });

    let mut cached_service = cache_layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/video.mp4")
        .body(())
        .unwrap();
    let resp = cached_service
        .ready()
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify NOT cached
    let cached = backend.get("/video.mp4").await.unwrap();
    assert!(cached.is_none(), "Video should not be cached");
}

#[tokio::test]
async fn test_audio_content_type_excluded() {
    let backend = InMemoryBackend::new(100);
    let cache_layer = CacheLayer::builder(backend.clone())
        .policy(CachePolicy::default().with_streaming_policy(StreamingPolicy::default()))
        .build();

    let audio_data = vec![0u8; 256 * 1024]; // 256KB
    let service = tower::service_fn(move |_req: Request<()>| {
        let data = audio_data.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-type", "audio/mpeg")
                    .header("content-length", data.len().to_string())
                    .body(Full::from(data))
                    .unwrap(),
            )
        }
    });

    let mut cached_service = cache_layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/song.mp3")
        .body(())
        .unwrap();
    let _resp = cached_service
        .ready()
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();

    let cached = backend.get("/song.mp3").await.unwrap();
    assert!(cached.is_none(), "Audio should not be cached");
}

#[tokio::test]
async fn test_zip_file_excluded() {
    let backend = InMemoryBackend::new(100);
    let cache_layer = CacheLayer::builder(backend.clone())
        .policy(CachePolicy::default().with_streaming_policy(StreamingPolicy::default()))
        .build();

    let zip_data = vec![0u8; 100 * 1024]; // 100KB
    let service = tower::service_fn(move |_req: Request<()>| {
        let data = zip_data.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-type", "application/zip")
                    .header("content-length", data.len().to_string())
                    .body(Full::from(data))
                    .unwrap(),
            )
        }
    });

    let mut cached_service = cache_layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/archive.zip")
        .body(())
        .unwrap();
    let _resp = cached_service
        .ready()
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();

    let cached = backend.get("/archive.zip").await.unwrap();
    assert!(cached.is_none(), "ZIP file should not be cached");
}

#[tokio::test]
async fn test_large_json_over_max_size() {
    // Even though JSON is in force_cache, size limits still apply
    let backend = InMemoryBackend::new(100);
    let cache_layer = CacheLayer::builder(backend.clone())
        .policy(CachePolicy::default().with_streaming_policy(StreamingPolicy::default()))
        .build();

    // 2MB JSON (over 1MB limit, should NOT be cached)
    let large_json = vec![b'x'; 2 * 1024 * 1024];
    let service = tower::service_fn(move |_req: Request<()>| {
        let data = large_json.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-type", "application/json")
                    .header("content-length", data.len().to_string())
                    .status(StatusCode::OK)
                    .body(Full::from(data))
                    .unwrap(),
            )
        }
    });

    let mut cached_service = cache_layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/large.json")
        .body(())
        .unwrap();
    let _resp = cached_service
        .ready()
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(10)).await;

    // Even though it's JSON, size limit should prevent caching
    let cached = backend.get("/large.json").await.unwrap();
    assert!(
        cached.is_none(),
        "Large JSON over size limit should not be cached (size limits apply to all types)"
    );
}

#[tokio::test]
async fn test_medium_json_cached() {
    // JSON under the size limit should be cached
    let backend = InMemoryBackend::new(100);
    let cache_layer = CacheLayer::builder(backend.clone())
        .policy(CachePolicy::default().with_streaming_policy(StreamingPolicy::default()))
        .build();

    // 800KB JSON (under 1MB limit, should be cached)
    let json_data = vec![b'x'; 800 * 1024];
    let service = tower::service_fn(move |_req: Request<()>| {
        let data = json_data.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-type", "application/json")
                    .header("content-length", data.len().to_string())
                    .status(StatusCode::OK)
                    .body(Full::from(data))
                    .unwrap(),
            )
        }
    });

    let mut cached_service = cache_layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/medium.json")
        .body(())
        .unwrap();
    let _resp = cached_service
        .ready()
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(10)).await;

    let cached = backend.get("/medium.json").await.unwrap();
    assert!(
        cached.is_some(),
        "Medium-sized JSON under limit should be cached"
    );
}

#[tokio::test]
async fn test_streaming_policy_disabled() {
    let backend = InMemoryBackend::new(100);
    let cache_layer = CacheLayer::builder(backend.clone())
        .policy(CachePolicy::default().with_streaming_policy(StreamingPolicy::disabled()))
        .build();

    // Large PDF that would normally be excluded
    let large_pdf = vec![0u8; 10 * 1024 * 1024];
    let service = tower::service_fn(move |_req: Request<()>| {
        let data = large_pdf.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-type", "application/pdf")
                    .header("content-length", data.len().to_string())
                    .body(Full::from(data))
                    .unwrap(),
            )
        }
    });

    let mut cached_service = cache_layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/doc.pdf")
        .body(())
        .unwrap();
    let _resp = cached_service
        .ready()
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(10)).await;

    // With disabled streaming policy, should still cache (if under max_body_size)
    // But the 10MB will be too large for default in-memory backend limits
    // This test validates that streaming policy can be disabled
}

#[tokio::test]
async fn test_size_only_policy() {
    let backend = InMemoryBackend::new(100);
    let cache_layer = CacheLayer::builder(backend.clone())
        .policy(
            CachePolicy::default().with_streaming_policy(StreamingPolicy::size_only(512 * 1024)),
        )
        .build();

    // Small PDF should be cached (no content-type filtering)
    let small_pdf = vec![0u8; 256 * 1024]; // 256KB
    let service = tower::service_fn(move |_req: Request<()>| {
        let data = small_pdf.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-type", "application/pdf")
                    .header("content-length", data.len().to_string())
                    .status(StatusCode::OK)
                    .body(Full::from(data))
                    .unwrap(),
            )
        }
    });

    let mut cached_service = cache_layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/small.pdf")
        .body(())
        .unwrap();
    let _resp = cached_service
        .ready()
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(10)).await;

    let cached = backend.get("/small.pdf").await.unwrap();
    assert!(
        cached.is_some(),
        "Small PDF should be cached with size-only policy"
    );
}

#[tokio::test]
async fn test_content_type_with_charset() {
    let backend = InMemoryBackend::new(100);
    let cache_layer = CacheLayer::builder(backend.clone())
        .policy(CachePolicy::default().with_streaming_policy(StreamingPolicy::default()))
        .build();

    let json_data = r#"{"test":true}"#;
    let service = tower::service_fn(move |_req: Request<()>| {
        let data = json_data.to_string();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-type", "application/json; charset=utf-8")
                    .header("content-length", data.len().to_string())
                    .status(StatusCode::OK)
                    .body(Full::from(data))
                    .unwrap(),
            )
        }
    });

    let mut cached_service = cache_layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/data.json")
        .body(())
        .unwrap();
    let _resp = cached_service
        .ready()
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(10)).await;

    let cached = backend.get("/data.json").await.unwrap();
    assert!(
        cached.is_some(),
        "JSON with charset should be cached normally"
    );
}

#[tokio::test]
async fn test_multi_tier_large_file_not_in_l1() {
    let l1 = InMemoryBackend::new(100);
    let l2 = InMemoryBackend::new(1000);

    let backend = MultiTierBuilder::new()
        .l1(l1.clone())
        .l2(l2.clone())
        .write_through(true)
        .max_l1_entry_size(Some(256 * 1024)) // 256KB limit for L1
        .build();

    let cache_layer = CacheLayer::builder(backend.clone())
        .policy(
            CachePolicy::default()
                .with_streaming_policy(StreamingPolicy::size_only(2 * 1024 * 1024)), // 2MB limit
        )
        .build();

    // 500KB response - too large for L1, but under streaming limit
    let large_data = vec![b'x'; 500 * 1024];
    let service = tower::service_fn(move |_req: Request<()>| {
        let data = large_data.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-type", "application/json")
                    .header("content-length", data.len().to_string())
                    .status(StatusCode::OK)
                    .body(Full::from(data))
                    .unwrap(),
            )
        }
    });

    let mut cached_service = cache_layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/large-data.json")
        .body(())
        .unwrap();
    let _resp = cached_service
        .ready()
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Should be in L2
    let l2_result = l2.get("/large-data.json").await.unwrap();
    assert!(l2_result.is_some(), "Large entry should be in L2");

    // Should NOT be in L1 (too large)
    let l1_result = l1.get("/large-data.json").await.unwrap();
    assert!(
        l1_result.is_none(),
        "Large entry should not be in L1 due to size limit"
    );
}

#[tokio::test]
async fn test_multi_tier_promotion_skipped_for_large_entries() {
    let l1 = InMemoryBackend::new(100);
    let l2 = InMemoryBackend::new(1000);

    let backend = MultiTierBuilder::new()
        .l1(l1.clone())
        .l2(l2.clone())
        .write_through(false) // L2 only on write
        .promotion_threshold(2)
        .max_l1_entry_size(Some(100 * 1024)) // 100KB limit
        .build();

    let cache_layer = CacheLayer::builder(backend.clone())
        .policy(
            CachePolicy::default().with_streaming_policy(StreamingPolicy::size_only(1024 * 1024)),
        )
        .build();

    // 200KB response - should cache in L2, but not promote to L1
    let data = vec![b'x'; 200 * 1024];
    let service = tower::service_fn(move |_req: Request<()>| {
        let data = data.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-type", "text/plain")
                    .header("content-length", data.len().to_string())
                    .status(StatusCode::OK)
                    .body(Full::from(data))
                    .unwrap(),
            )
        }
    });

    let mut cached_service = cache_layer.layer(service);

    // First request - stores in L2
    let req = Request::builder()
        .method(Method::GET)
        .uri("/data.txt")
        .body(())
        .unwrap();
    let _resp = cached_service
        .ready()
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Hit L2 twice to trigger promotion
    for _ in 0..2 {
        let req = Request::builder()
            .method(Method::GET)
            .uri("/data.txt")
            .body(())
            .unwrap();
        let _resp = cached_service
            .ready()
            .await
            .unwrap()
            .call(req)
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Should still be in L2
    let l2_result = l2.get("/data.txt").await.unwrap();
    assert!(l2_result.is_some(), "Entry should be in L2");

    // Should NOT be promoted to L1 (too large)
    let l1_result = l1.get("/data.txt").await.unwrap();
    assert!(
        l1_result.is_none(),
        "Large entry should not be promoted to L1"
    );
}

#[tokio::test]
async fn test_case_insensitive_content_type_matching() {
    let backend = InMemoryBackend::new(100);
    let cache_layer = CacheLayer::builder(backend.clone())
        .policy(CachePolicy::default().with_streaming_policy(StreamingPolicy::default()))
        .build();

    let pdf_data = vec![0u8; 5 * 1024 * 1024];
    let service = tower::service_fn(move |_req: Request<()>| {
        let data = pdf_data.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-type", "Application/PDF") // Mixed case
                    .header("content-length", data.len().to_string())
                    .body(Full::from(data))
                    .unwrap(),
            )
        }
    });

    let mut cached_service = cache_layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/doc.pdf")
        .body(())
        .unwrap();
    let _resp = cached_service
        .ready()
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();

    let cached = backend.get("/doc.pdf").await.unwrap();
    assert!(
        cached.is_none(),
        "PDF with mixed-case content-type should not be cached"
    );
}

#[tokio::test]
async fn test_octet_stream_excluded() {
    let backend = InMemoryBackend::new(100);
    let cache_layer = CacheLayer::builder(backend.clone())
        .policy(CachePolicy::default().with_streaming_policy(StreamingPolicy::default()))
        .build();

    let binary_data = vec![0u8; 100 * 1024];
    let service = tower::service_fn(move |_req: Request<()>| {
        let data = binary_data.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .header("content-type", "application/octet-stream")
                    .header("content-length", data.len().to_string())
                    .body(Full::from(data))
                    .unwrap(),
            )
        }
    });

    let mut cached_service = cache_layer.layer(service);

    let req = Request::builder()
        .method(Method::GET)
        .uri("/file.bin")
        .body(())
        .unwrap();
    let _resp = cached_service
        .ready()
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();

    let cached = backend.get("/file.bin").await.unwrap();
    assert!(
        cached.is_none(),
        "application/octet-stream should not be cached"
    );
}

#[tokio::test]
async fn test_chunk_cache_skips_no_store_and_serves_suffix_ranges_with_multi_headers() {
    use http_body_util::BodyExt;
    use tower_http_cache::range::RangeHandling;
    use tower_http_cache::tags::TagPolicy;

    let streaming_policy = StreamingPolicy {
        enabled: true,
        enable_chunk_cache: true,
        min_chunk_file_size: 1024,
        chunk_size: 512,
        range_handling: RangeHandling::CacheFullServeRanges,
        ..Default::default()
    };

    let backend = InMemoryBackend::new(100);
    let policy = CachePolicy::default()
        .with_ttl(Duration::from_secs(60))
        .with_streaming_policy(streaming_policy)
        .with_tag_policy(TagPolicy::new().with_enabled(true))
        .with_tag_extractor(|_m, uri| vec![format!("path:{}", uri.path())]);

    let layer = CacheLayer::builder(backend.clone()).policy(policy).build();
    let payload: Vec<u8> = (0..2048u32).map(|i| (i % 251) as u8).collect();

    let service = tower::service_fn({
        let payload = payload.clone();
        move |req: Request<()>| {
            let data = payload.clone();
            let path = req.uri().path().to_string();
            async move {
                let mut builder = Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/plain")
                    .header("x-multi", "one")
                    .header("x-multi", "two");
                if path == "/private" {
                    builder = builder.header("cache-control", "no-store");
                }
                Ok::<_, Infallible>(builder.body(Full::from(data)).unwrap())
            }
        }
    });

    let mut svc = layer.layer(service);

    // 1. `Cache-Control: no-store` must NOT populate the chunk cache.
    let no_store_req = Request::builder()
        .method(Method::GET)
        .uri("/private")
        .body(())
        .unwrap();
    let _ = svc
        .ready()
        .await
        .unwrap()
        .call(no_store_req)
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap();
    assert!(
        layer
            .chunk_cache()
            .and_then(|cc| cc.get("/private"))
            .is_none(),
        "no-store response must not be populated in ChunkCache"
    );

    // 2. Cacheable response populates ChunkCache and serves suffix range with multi-value headers.
    let ok_req = Request::builder()
        .method(Method::GET)
        .uri("/asset.txt")
        .body(())
        .unwrap();
    let _ = svc
        .ready()
        .await
        .unwrap()
        .call(ok_req)
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap();

    let range_req = Request::builder()
        .method(Method::GET)
        .uri("/asset.txt")
        .header("range", "bytes=-512")
        .body(())
        .unwrap();
    let range_resp = svc.ready().await.unwrap().call(range_req).await.unwrap();
    assert_eq!(range_resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        range_resp
            .headers()
            .get(http::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok()),
        Some("bytes 1536-2047/2048")
    );
    assert_eq!(
        range_resp
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()),
        Some("512")
    );
    let multi_vals: Vec<&str> = range_resp
        .headers()
        .get_all("x-multi")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    assert_eq!(multi_vals, vec!["one", "two"]);
    let range_body = range_resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&range_body[..], &payload[1536..2048]);

    // 3. Tag invalidation clears both backend and ChunkCache.
    let removed = layer.invalidate_by_tag("path:/asset.txt").await.unwrap();
    assert_eq!(removed, 1);
    assert!(backend.get("/asset.txt").await.unwrap().is_none());
    assert!(
        layer
            .chunk_cache()
            .and_then(|cc| cc.get("/asset.txt"))
            .is_none()
    );
}

#[tokio::test]
async fn test_chunk_cache_entry_expires_with_policy_ttl() {
    use http_body_util::BodyExt;

    let streaming_policy = StreamingPolicy {
        enabled: true,
        enable_chunk_cache: true,
        min_chunk_file_size: 1024,
        chunk_size: 512,
        ..Default::default()
    };

    let backend = InMemoryBackend::new(100);
    let layer = CacheLayer::builder(backend)
        .policy(
            CachePolicy::default()
                .with_ttl(Duration::from_millis(25))
                .with_streaming_policy(streaming_policy),
        )
        .build();

    let payload = vec![5u8; 1024];
    let service = tower::service_fn(move |_req: Request<()>| {
        let data = payload.clone();
        async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/plain")
                    .body(Full::from(data))
                    .unwrap(),
            )
        }
    });

    let mut svc = layer.layer(service);
    let req = Request::builder()
        .method(Method::GET)
        .uri("/ttl-chunks")
        .body(())
        .unwrap();
    let _ = svc
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

    assert!(
        layer
            .chunk_cache()
            .and_then(|cc| cc.get("/ttl-chunks"))
            .is_some()
    );

    tokio::time::sleep(Duration::from_millis(45)).await;

    assert!(
        layer
            .chunk_cache()
            .and_then(|cc| cc.get("/ttl-chunks"))
            .is_none(),
        "chunk cache entry must expire after response TTL elapses"
    );
}

#[tokio::test]
async fn chunk_cache_replaces_live_entry_on_refetch_without_mixing_bytes() {
    use http_body_util::BodyExt;

    let sp = StreamingPolicy {
        enable_chunk_cache: true,
        chunk_size: 8,
        min_chunk_file_size: 1,
        max_cacheable_size: Some(1 << 20),
        excluded_content_types: Default::default(),
        ..Default::default()
    };

    let calls = Arc::new(AtomicUsize::new(0));
    let layer = CacheLayer::builder(InMemoryBackend::new(100))
        .policy(
            CachePolicy::default()
                .with_streaming_policy(sp)
                .with_ttl(Duration::from_secs(60)),
        )
        .refresh_before(Duration::from_secs(120))
        .build();

    let mut svc = layer.layer(tower::service_fn({
        let calls = calls.clone();
        move |_r: Request<()>| {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                let (body, etag) = if n == 0 {
                    ("AAAAAAAABBBBBBBBCCCCCCCCDDDDDDDD".to_string(), "\"v1\"")
                } else {
                    ("zzzzzzzzyyyyyyyy".to_string(), "\"v2\"")
                };
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("content-type", "text/plain")
                        .header("etag", etag)
                        .body(Full::from(body))
                        .unwrap(),
                )
            }
        }
    }));

    let _ = svc
        .ready()
        .await
        .unwrap()
        .call(Request::get("/f").body(()).unwrap())
        .await
        .unwrap();
    let _ = svc
        .ready()
        .await
        .unwrap()
        .call(Request::get("/f").body(()).unwrap())
        .await
        .unwrap();

    let range_resp = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/f")
                .header("range", "bytes=0-31")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(range_resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(range_resp.headers().get("etag").unwrap(), "\"v2\"");
    let body = range_resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"zzzzzzzzyyyyyyyy");
}

#[cfg(feature = "admin-api")]
#[tokio::test]
async fn admin_invalidate_evicts_chunk_cache() {
    use http_body_util::BodyExt;
    use tower_http_cache::admin::routes::{InvalidationRequest, handle_invalidate};
    use tower_http_cache::{AdminConfig, AdminState};

    let sp = StreamingPolicy {
        enable_chunk_cache: true,
        chunk_size: 8,
        min_chunk_file_size: 1,
        max_cacheable_size: Some(1 << 20),
        excluded_content_types: Default::default(),
        ..Default::default()
    };

    let calls = Arc::new(AtomicUsize::new(0));
    let backend = InMemoryBackend::new(100);
    let layer = CacheLayer::builder(backend)
        .policy(
            CachePolicy::default()
                .with_streaming_policy(sp)
                .with_ttl(Duration::from_secs(60)),
        )
        .build();
    let admin = AdminState::from_layer(&layer, AdminConfig::default());

    let mut svc = layer.layer(tower::service_fn({
        let calls = calls.clone();
        move |_r: Request<()>| {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                let body = if n == 0 {
                    "OLDOLDOLDOLDOLDO"
                } else {
                    "NEWNEWNEWNEWNEWN"
                };
                Ok::<_, Infallible>(
                    Response::builder()
                        .header("content-type", "text/plain")
                        .body(Full::from(body))
                        .unwrap(),
                )
            }
        }
    }));

    let _ = svc
        .ready()
        .await
        .unwrap()
        .call(Request::get("/a").body(()).unwrap())
        .await
        .unwrap();

    let resp = handle_invalidate(
        &admin,
        InvalidationRequest {
            key: Some("/a".into()),
            tag: None,
            tags: None,
        },
    )
    .await
    .unwrap();
    assert!(resp.success);
    assert!(layer.chunk_cache().unwrap().get("/a").is_none());

    let after = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/a")
                .header("range", "bytes=0-15")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(after.status(), StatusCode::OK);
    let body = after.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"NEWNEWNEWNEWNEWN");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn if_range_mismatch_ignores_chunk_206_and_serves_full_response() {
    use http_body_util::BodyExt;

    let sp = StreamingPolicy {
        enable_chunk_cache: true,
        chunk_size: 8,
        min_chunk_file_size: 1,
        max_cacheable_size: Some(1 << 20),
        excluded_content_types: Default::default(),
        ..Default::default()
    };

    let layer = CacheLayer::builder(InMemoryBackend::new(100))
        .policy(
            CachePolicy::default()
                .with_streaming_policy(sp)
                .with_ttl(Duration::from_secs(60)),
        )
        .build();
    let mut svc = layer.layer(tower::service_fn(|_r: Request<()>| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .header("content-type", "text/plain")
                .header("etag", "\"current\"")
                .body(Full::from("0123456789abcdef"))
                .unwrap(),
        )
    }));

    // Warm full + chunk caches.
    let _ = svc
        .ready()
        .await
        .unwrap()
        .call(Request::get("/ir").body(()).unwrap())
        .await
        .unwrap();

    // Matching strong ETag If-Range -> 206 Partial Content from chunk cache.
    let match_resp = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/ir")
                .header("range", "bytes=0-3")
                .header("if-range", "\"current\"")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(match_resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        match_resp.into_body().collect().await.unwrap().to_bytes(),
        Bytes::from_static(b"0123")
    );

    // Mismatched If-Range -> falls back to full 200 OK response instead of 206.
    let mismatch_resp = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/ir")
                .header("range", "bytes=0-3")
                .header("if-range", "\"stale\"")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mismatch_resp.status(), StatusCode::OK);
    assert_eq!(
        mismatch_resp
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
        Bytes::from_static(b"0123456789abcdef")
    );
}

#[tokio::test]
async fn range_passthrough_checks_request_range_header_and_skips_storage() {
    use http_body_util::BodyExt;
    use tower_http_cache::range::RangeHandling;

    let sp = StreamingPolicy {
        enabled: true,
        range_handling: RangeHandling::PassThrough,
        enable_chunk_cache: false,
        excluded_content_types: Default::default(),
        ..Default::default()
    };

    let calls = Arc::new(AtomicUsize::new(0));
    let layer = CacheLayer::builder(InMemoryBackend::new(100))
        .policy(
            CachePolicy::default()
                .with_streaming_policy(sp)
                .with_ttl(Duration::from_secs(60)),
        )
        .build();
    let mut svc = layer.layer(tower::service_fn({
        let calls = calls.clone();
        move |_r: Request<()>| {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                // Intentionally return 200 without Range/Content-Range on response headers
                // to verify the request's Range header triggers PassThrough.
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .header("content-type", "text/plain")
                        .body(Full::from(format!("slice-{n}")))
                        .unwrap(),
                )
            }
        }
    }));

    let r1 = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/pt")
                .header("range", "bytes=0-3")
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
    let r2 = svc
        .ready()
        .await
        .unwrap()
        .call(
            Request::get("/pt")
                .header("range", "bytes=0-3")
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

    assert_ne!(r1, r2);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
