//! Production Memcached deployment with connection pooling
//!
//! Demonstrates:
//! - BB8 connection pool configuration
//! - Multi-server setup readiness
//! - Performance monitoring
//! - Production best practices
//!
//! Setup:
//!   docker run -d -p 11211:11211 memcached
//!
//! Run: cargo run --example memcached_production --features memcached-backend

#[cfg(not(feature = "memcached-backend"))]
fn main() {
    eprintln!("This example requires the 'memcached-backend' feature.");
    eprintln!("Run with: cargo run --example memcached_production --features memcached-backend");
    std::process::exit(1);
}

#[cfg(feature = "memcached-backend")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;
    use tower_http_cache::backend::memcached::MemcachedBackend;
    use tower_http_cache::prelude::*;
    use tower_http_cache::streaming::StreamingPolicy;

    println!("🚀 Memcached Production Setup Example");
    println!("======================================\n");

    // Step 1: Create connection pool
    println!("📡 Connecting to Memcached...");

    let backend = MemcachedBackend::builder()
        .address("127.0.0.1:11211")
        .namespace("myapp_prod")
        .max_connections(20) // Pool size for production load
        .min_connections(5) // Keep minimum connections warm
        .connection_timeout(Duration::from_secs(5))
        .build()
        .await?;

    println!("✅ Connected to Memcached with connection pool");

    // Step 2: Show pool configuration
    let state = backend.pool_state();
    println!("\n📊 Pool Configuration:");
    println!("   Connections: {}", state.connections);
    println!("   Idle: {}", state.idle_connections);
    println!("   Max size: 20");
    println!("   Min size: 5");

    // Step 3: Create cache layer with production settings
    println!("\n⚙️  Configuring cache policy...");

    let cache_layer = CacheLayer::builder(backend.clone())
        .policy(
            CachePolicy::default()
                .with_ttl(Duration::from_secs(300)) // 5 minutes
                .with_negative_ttl(Duration::from_secs(60)) // 1 minute for errors
                .with_stale_while_revalidate(Duration::from_secs(30))
                .with_streaming_policy(StreamingPolicy {
                    enabled: true,
                    max_cacheable_size: Some(1024 * 1024), // 1MB max per entry
                    enable_chunk_cache: false, // Disabled for Memcached (use in-memory for chunks)
                    ..Default::default()
                })
                .with_respect_cache_control(true),
        )
        .build();

    println!("✅ Cache layer configured");

    // Step 4: Production best practices
    println!("\n📝 Production Best Practices:");
    println!("   ✓ Connection pooling (20 max, 5 min)");
    println!("   ✓ Namespace isolation (myapp_prod)");
    println!("   ✓ TTL configuration (5m cache, 1m negative)");
    println!("   ✓ Stale-while-revalidate (30s grace period)");
    println!("   ✓ Size limits (1MB max per entry)");
    println!("   ✓ Cache-Control header respect");

    // Step 5: Health check
    println!("\n🏥 Running health check...");

    let test_key = "health_check";
    let test_value = "ok";

    // Write test
    use bytes::Bytes;
    use http::StatusCode;
    use tower_http_cache::backend::{CacheBackend, CacheEntry};

    let entry = CacheEntry::new(
        StatusCode::OK,
        http::Version::HTTP_11,
        vec![],
        Bytes::from(test_value),
    );

    backend
        .set(
            test_key.to_string(),
            entry,
            Duration::from_secs(10),
            Duration::ZERO,
        )
        .await?;

    // Read test
    let result = backend.get(test_key).await?;

    if result.is_some() {
        println!("✅ Health check passed (write + read successful)");
    } else {
        println!("❌ Health check failed (could not read back value)");
        return Err("Health check failed".into());
    }

    // Cleanup
    backend.invalidate(test_key).await?;
    println!("✅ Cleanup successful");

    // Step 6: Performance tips
    println!("\n⚡ Performance Tips:");
    println!("   1. Use connection pooling (already configured)");
    println!("   2. Set appropriate TTLs based on data freshness needs");
    println!("   3. Use stale-while-revalidate for better UX");
    println!("   4. Monitor cache hit rates and adjust policies");
    println!("   5. Consider multi-tier caching (in-memory L1 + Memcached L2)");

    // Step 7: Multi-server setup example
    println!("\n🌐 Multi-Server Setup:");
    println!("   For HA deployments, run multiple Memcached instances:");
    println!("   ```");
    println!("   docker run -d -p 11211:11211 memcached");
    println!("   docker run -d -p 11212:11212 memcached");
    println!("   ```");
    println!("   Then use a load balancer or client-side hashing");

    // Step 8: Monitoring recommendations
    println!("\n📈 Monitoring Recommendations:");
    println!("   - Track: cache hit rate, miss rate, latency");
    println!("   - Alert on: connection pool exhaustion, high error rates");
    println!("   - Log: cache invalidations, stale serves");
    println!("   - Metrics: Use the 'metrics' feature for Prometheus export");

    // Step 9: Show configuration summary
    println!("\n📋 Configuration Summary:");
    println!("   Backend: Memcached");
    println!("   Address: 127.0.0.1:11211");
    println!("   Namespace: myapp_prod");
    println!("   Pool: 5-20 connections");
    println!("   TTL: 5m (cache), 1m (negative)");
    println!("   Max entry size: 1MB");
    println!("   Stale grace period: 30s");

    println!("\n✅ Memcached is ready for production traffic!");
    println!("\nPress Ctrl+C to exit...");

    // Keep alive for inspection
    tokio::signal::ctrl_c().await?;

    println!("\n👋 Shutting down gracefully...");

    Ok(())
}
