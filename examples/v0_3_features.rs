//! Example demonstrating v0.3.0 features:
//! - Cache Tags & Invalidation Groups
//! - Multi-Tier Caching (L1 + L2)
//! - ML-Ready Structured Logging
//! - Request ID Propagation

use bytes::Bytes;
use http::{Method, Request, Response, StatusCode};
use http_body_util::Full;
use std::time::Duration;
use tower::{Service, ServiceBuilder, ServiceExt};
use tower_http_cache::prelude::*;
use tower_http_cache::{
    backend::multi_tier::{MultiTierBackend, PromotionStrategy},
    logging::{CacheEvent, CacheEventType, MLLoggingConfig},
    request_id::RequestId,
    tags::TagPolicy,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing for observability
    #[cfg(feature = "tracing")]
    {
        tracing_subscriber::fmt()
            .with_env_filter("tower_http_cache=debug")
            .init();
    }

    println!("=== tower-http-cache v0.3.0 Features Demo ===\n");

    // Demo 1: Cache Tags
    println!("1. Cache Tags & Invalidation Groups");
    demo_cache_tags().await?;

    // Demo 2: Multi-Tier Caching
    println!("\n2. Multi-Tier Caching (L1 + L2)");
    demo_multi_tier().await?;

    // Demo 3: ML-Ready Logging
    println!("\n3. ML-Ready Structured Logging");
    demo_ml_logging().await?;

    // Demo 4: Request ID Propagation
    println!("\n4. Request ID Propagation");
    demo_request_id().await?;

    // Demo 5: Combined Features
    println!("\n5. All Features Combined");
    demo_combined_features().await?;

    Ok(())
}

async fn demo_cache_tags() -> Result<(), Box<dyn std::error::Error>> {
    // Create backend with tag support
    let backend = InMemoryBackend::new(1000);

    // Configure tag policy
    let tag_policy = TagPolicy::new()
        .with_enabled(true)
        .with_max_tags_per_entry(5);

    // Build cache layer with tag extractor
    let layer = CacheLayer::builder(backend.clone())
        .ttl(Duration::from_secs(300))
        .policy(
            CachePolicy::default()
                .with_tag_policy(tag_policy)
                .with_tag_extractor(|method, uri| {
                    // Extract tags from URI path
                    if method == &Method::GET {
                        let path = uri.path();
                        if path.starts_with("/users/") {
                            let parts: Vec<&str> = path.split('/').collect();
                            if parts.len() >= 3 {
                                return vec![format!("user:{}", parts[2]), "users".to_string()];
                            }
                        }
                    }
                    vec![]
                }),
        )
        .build();

    // Create service
    let service = ServiceBuilder::new()
        .layer(layer)
        .service(tower::service_fn(|_req: Request<()>| async {
            Ok::<_, std::convert::Infallible>(Response::new(Full::from("User data")))
        }));

    // Make requests that will be tagged
    let req1 = Request::builder().uri("/users/123").body(()).unwrap();

    let req2 = Request::builder().uri("/users/456").body(()).unwrap();

    let _resp1 = service.clone().oneshot(req1).await?;
    let _resp2 = service.clone().oneshot(req2).await?;

    println!("  ✓ Cached 2 user entries with tags");
    println!("  ✓ Tags: user:123, user:456, users");

    // Invalidate by tag
    let count = backend.invalidate_by_tag("users").await?;
    println!("  ✓ Invalidated {} entries by tag 'users'", count);

    Ok(())
}

async fn demo_multi_tier() -> Result<(), Box<dyn std::error::Error>> {
    // Create L1 (fast, small) and L2 (slower, large) backends
    let l1 = InMemoryBackend::new(100); // Small fast cache
    let l2 = InMemoryBackend::new(10_000); // Large persistent cache

    // Build multi-tier backend with promotion strategy
    let backend = MultiTierBackend::builder()
        .l1(l1.clone())
        .l2(l2.clone())
        .promotion_strategy(PromotionStrategy::HitCount { threshold: 3 })
        .write_through(true)
        .build();

    // Create service
    let layer = CacheLayer::builder(backend.clone())
        .ttl(Duration::from_secs(300))
        .build();

    let mut service = ServiceBuilder::new()
        .layer(layer)
        .service(tower::service_fn(|_req: Request<()>| async {
            Ok::<_, std::convert::Infallible>(Response::new(Full::from("Data")))
        }));

    let req = Request::builder().uri("/api/hot-data").body(()).unwrap();

    // First request: stored in both L1 and L2
    let _resp = service.ready().await?.call(req.clone()).await?;
    println!("  ✓ First request: stored in L1 and L2");

    // Simulate L1 eviction
    l1.invalidate("/api/hot-data").await?;
    println!("  ✓ Simulated L1 eviction");

    // Multiple requests to trigger promotion
    for i in 1..=3 {
        let _resp = service.ready().await?.call(req.clone()).await?;
        println!("  ✓ Request {}: L2 hit", i);
    }

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Check if promoted back to L1
    if l1.get("/api/hot-data").await?.is_some() {
        println!("  ✓ Hot data promoted back to L1 after threshold");
    }

    Ok(())
}

async fn demo_ml_logging() -> Result<(), Box<dyn std::error::Error>> {
    // Configure ML-ready structured logging
    let ml_config = MLLoggingConfig::new()
        .with_enabled(true)
        .with_sample_rate(1.0) // Log all events for demo
        .with_hash_keys(true); // Hash keys for privacy

    let backend = InMemoryBackend::new(1000);

    let layer = CacheLayer::builder(backend)
        .policy(CachePolicy::default().with_ml_logging(ml_config))
        .build();

    // Create a request ID
    let request_id = RequestId::new();
    println!("  ✓ Request ID: {}", request_id);

    // Log a cache event manually
    let event = CacheEvent::new(
        CacheEventType::Hit,
        request_id.clone(),
        "/api/users/123".to_string(),
    )
    .with_hit(true)
    .with_latency(Duration::from_micros(150))
    .with_size(1024)
    .with_ttl(Duration::from_secs(300))
    .with_tags(vec!["user:123".to_string(), "users".to_string()])
    .with_tier("l1");

    event.log(&MLLoggingConfig::new().with_enabled(true));
    println!("  ✓ Logged cache hit event with full metadata");
    println!("  ✓ Event includes: latency, size, TTL, tags, tier");

    Ok(())
}

async fn demo_request_id() -> Result<(), Box<dyn std::error::Error>> {
    // Create a request with X-Request-ID header
    let request_id = RequestId::new();

    let mut req = Request::builder().uri("/api/data").body(()).unwrap();

    req.headers_mut()
        .insert("x-request-id", request_id.as_str().parse().unwrap());

    println!("  ✓ Request with X-Request-ID: {}", request_id);

    // Extract request ID from request
    let extracted_id = RequestId::from_request(&req);
    assert_eq!(extracted_id.as_str(), request_id.as_str());
    println!("  ✓ Request ID successfully extracted from headers");

    // Auto-generate if missing
    let req_without_id = Request::builder().uri("/api/data").body(()).unwrap();

    let auto_id = RequestId::from_request(&req_without_id);
    println!("  ✓ Auto-generated request ID: {}", auto_id);

    Ok(())
}

async fn demo_combined_features() -> Result<(), Box<dyn std::error::Error>> {
    // Combine all features: multi-tier, tags, ML logging, request IDs

    let l1 = InMemoryBackend::new(100);
    let l2 = InMemoryBackend::new(10_000);

    let backend = MultiTierBackend::builder()
        .l1(l1)
        .l2(l2)
        .promotion_threshold(2)
        .build();

    let layer = CacheLayer::builder(backend.clone())
        .policy(
            CachePolicy::default()
                .with_ttl(Duration::from_secs(300))
                .with_tag_policy(
                    TagPolicy::new()
                        .with_enabled(true)
                        .with_max_tags_per_entry(10),
                )
                .with_tag_extractor(|_method, uri| vec![format!("path:{}", uri.path())])
                .with_ml_logging(
                    MLLoggingConfig::new()
                        .with_enabled(true)
                        .with_sample_rate(1.0),
                ),
        )
        .build();

    let mut service = ServiceBuilder::new()
        .layer(layer)
        .service(tower::service_fn(|_req: Request<()>| async {
            Ok::<_, std::convert::Infallible>(
                Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::from("Combined features response"))
                    .unwrap(),
            )
        }));

    // Make request with request ID
    let request_id = RequestId::new();
    let mut req = Request::builder().uri("/api/combined").body(()).unwrap();

    req.headers_mut()
        .insert("x-request-id", request_id.as_str().parse().unwrap());

    let _resp = service.ready().await?.call(req).await?;

    println!("  ✓ Request processed with all features:");
    println!("    - Request ID: {}", request_id);
    println!("    - Multi-tier caching (L1 + L2)");
    println!("    - Cache tags: path:/api/combined");
    println!("    - ML logging enabled");

    // Check tags
    let tags = backend.list_tags().await?;
    println!("  ✓ Tags in cache: {:?}", tags);

    // Get stats
    let stats = backend.stats();
    println!("  ✓ Tier stats available for monitoring");

    Ok(())
}
