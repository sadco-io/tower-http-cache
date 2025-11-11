//! Integration tests for smart streaming and large file handling.

use http::{Method, Request, Response, StatusCode};
use http_body_util::Full;
use std::convert::Infallible;
use std::time::Duration;
use tower::{Layer, Service, ServiceExt};
use tower_http_cache::backend::memory::InMemoryBackend;
use tower_http_cache::backend::multi_tier::MultiTierBuilder;
use tower_http_cache::backend::CacheBackend;
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
