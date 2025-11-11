//! Chunk caching demonstration for large files and range requests
//!
//! This example shows how to configure chunk caching for efficient
//! handling of large files with range request support.
//!
//! Run: cargo run --example chunk_cache_demo

use http::{Method, Request, Response, StatusCode, Uri};
use http_body_util::Full;
use std::collections::HashSet;
use std::time::Duration;
use tower::{Service, ServiceBuilder, ServiceExt};
use tower_http_cache::prelude::*;
use tower_http_cache::streaming::StreamingPolicy;
use bytes::Bytes;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🎬 Chunk Cache Demonstration\n");
    println!("================================\n");

    // Step 1: Configure cache with chunk caching enabled
    println!("Step 1: Configuring chunk cache...");

    let backend = InMemoryBackend::new(500);

    let cache_layer = CacheLayer::builder(backend.clone())
        .policy(
            CachePolicy::default()
                .with_ttl(Duration::from_secs(3600))
                .with_streaming_policy(StreamingPolicy {
                    enabled: true,
                    max_cacheable_size: Some(100 * 1024 * 1024), // 100MB
                    enable_chunk_cache: true,                     // Enable chunk caching!
                    chunk_size: 1024 * 1024,                      // 1MB chunks
                    min_chunk_file_size: 5 * 1024 * 1024,         // 5MB minimum
                    excluded_content_types: HashSet::new(),
                    force_cache_content_types: HashSet::from([
                        "video/*".to_string(),
                        "audio/*".to_string(),
                    ]),
                    ..Default::default()
                }),
        )
        .build();

    println!("✅ Chunk cache configured:");
    println!("   - Enabled for files >= 5MB");
    println!("   - Chunk size: 1MB");
    println!("   - Max file size: 100MB\n");

    // Step 2: Create a service that returns a large file
    println!("Step 2: Creating mock video service...");

    let video_data = Bytes::from(vec![0u8; 10 * 1024 * 1024]); // 10MB video

    let service = ServiceBuilder::new()
        .layer(cache_layer)
        .service(tower::service_fn(move |_req: Request<()>| {
            let data = video_data.clone();
            async move {
                Ok::<_, std::convert::Infallible>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .header("content-type", "video/mp4")
                        .header("content-length", data.len().to_string())
                        .body(Full::from(data))
                        .unwrap(),
                )
            }
        }));

    println!("✅ Mock service created (returns 10MB video)\n");

    // Step 3: Make first request (populates chunk cache)
    println!("Step 3: Making first request (cache miss)...");

    let req1 = Request::builder()
        .method(Method::GET)
        .uri("/video.mp4")
        .body(())
        .unwrap();

    let resp1 = service.clone().oneshot(req1).await.map_err(|e| format!("{}", e))?;
    let (parts1, body1) = resp1.into_parts();

    use http_body_util::BodyExt;
    let bytes1 = body1.collect().await.map_err(|e| format!("{}", e))?.to_bytes();

    println!("✅ First request completed:");
    println!("   - Status: {}", parts1.status);
    println!("   - Size: {} bytes", bytes1.len());
    println!("   - File cached and split into chunks\n");

    // Step 4: Make range request (should hit chunk cache)
    println!("Step 4: Making range request (should hit chunk cache)...");

    let req2 = Request::builder()
        .method(Method::GET)
        .uri("/video.mp4")
        .header("range", "bytes=0-1048575") // First 1MB
        .body(())
        .unwrap();

    let resp2 = service.clone().oneshot(req2).await.map_err(|e| format!("{}", e))?;
    let (parts2, body2) = resp2.into_parts();
    let bytes2 = body2.collect().await.map_err(|e| format!("{}", e))?.to_bytes();

    println!("✅ Range request completed:");
    println!("   - Status: {}", parts2.status);
    println!("   - Size: {} bytes (requested 1MB)", bytes2.len());
    if parts2.status == StatusCode::PARTIAL_CONTENT {
        println!("   - ✨ 206 Partial Content (range request served from chunk cache!)");
    }

    // Check for Content-Range header
    if let Some(content_range) = parts2.headers.get("content-range") {
        println!("   - Content-Range: {}", content_range.to_str().unwrap());
    }

    println!();

    // Step 5: Make another range request for different chunk
    println!("Step 5: Making second range request (different range)...");

    let req3 = Request::builder()
        .method(Method::GET)
        .uri("/video.mp4")
        .header("range", "bytes=1048576-2097151") // Second 1MB
        .body(())
        .unwrap();

    let resp3 = service.clone().oneshot(req3).await.map_err(|e| format!("{}", e))?;
    let (parts3, body3) = resp3.into_parts();
    let bytes3 = body3.collect().await.map_err(|e| format!("{}", e))?.to_bytes();

    println!("✅ Second range request completed:");
    println!("   - Status: {}", parts3.status);
    println!("   - Size: {} bytes", bytes3.len());
    if let Some(content_range) = parts3.headers.get("content-range") {
        println!("   - Content-Range: {}", content_range.to_str().unwrap());
    }

    println!("\n================================");
    println!("✅ Demonstration complete!\n");
    println!("Key Benefits:");
    println!("  • First request caches and chunks the file");
    println!("  • Range requests hit chunk cache directly");
    println!("  • No need to re-download the entire file");
    println!("  • Efficient memory usage (only cache accessed chunks)");
    println!("  • Perfect for video streaming and large file downloads");

    Ok(())
}
