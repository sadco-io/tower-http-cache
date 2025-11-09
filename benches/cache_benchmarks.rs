use std::convert::Infallible;
use std::hint::black_box;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{Criterion, criterion_group, criterion_main};
use http::{Method, Request, Response, StatusCode, Uri, Version};
use http_body_util::Full;
use tokio::runtime::Runtime;
use tokio::time::sleep;
use tower::{Layer, Service, ServiceExt};
use tower_http_cache::backend::CacheEntry;
use tower_http_cache::prelude::*;

fn tokio_runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| Runtime::new().expect("failed to build Tokio runtime"))
}

fn request(path_and_query: &str) -> Request<()> {
    Request::builder()
        .method(Method::GET)
        .uri(path_and_query)
        .body(())
        .expect("valid request")
}

fn sample_entry(body_len: usize) -> CacheEntry {
    let mut headers = Vec::new();
    headers.push(("content-type".to_string(), b"application/json".to_vec()));
    let payload = vec![b'x'; body_len];

    CacheEntry::new(
        StatusCode::OK,
        Version::HTTP_11,
        headers,
        Bytes::from(payload),
    )
}

fn bench_layer_throughput(c: &mut Criterion) {
    let rt = tokio_runtime();
    let inner_counter = Arc::new(AtomicUsize::new(0));

    let inner_service = tower::service_fn({
        let counter = inner_counter.clone();
        move |_req: Request<()>| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::Relaxed);
                sleep(Duration::from_micros(200)).await;
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .header("content-type", "application/json")
                        .body(Full::from(Bytes::from_static(b"{\"ok\":true}")))
                        .unwrap(),
                )
            }
        }
    });

    let hit_layer = CacheLayer::builder(InMemoryBackend::new(50_000))
        .ttl(Duration::from_secs(60))
        .stale_while_revalidate(Duration::from_secs(10))
        .refresh_before(Duration::from_secs(1))
        .build();

    let miss_layer = CacheLayer::builder(InMemoryBackend::new(50_000))
        .ttl(Duration::from_secs(60))
        .stale_while_revalidate(Duration::from_secs(10))
        .refresh_before(Duration::from_secs(1))
        .build();

    let hit_request = request("/catalog/items/42?locale=en-US");
    rt.block_on({
        let mut warm_service = hit_layer.layer(inner_service.clone());
        let req = hit_request.clone();
        async move {
            warm_service.ready().await.unwrap();
            let _ = warm_service.call(req).await.unwrap();
        }
    });

    let mut baseline_service = inner_service.clone();
    let mut cached_hit_service = hit_layer.layer(inner_service.clone());
    let mut cached_miss_service = miss_layer.layer(inner_service.clone());

    let miss_requests: Vec<Request<()>> = (0..512)
        .map(|i| request(&format!("/catalog/items/{i}?locale=en-US")))
        .collect();
    let miss_cursor = Arc::new(AtomicUsize::new(0));

    c.bench_function("layer_throughput/baseline_inner", |b| {
        b.iter(|| {
            rt.block_on(async {
                baseline_service.ready().await.unwrap();
                let resp = baseline_service.call(hit_request.clone()).await.unwrap();
                black_box(resp.status());
            });
        });
    });

    c.bench_function("layer_throughput/cache_hit", |b| {
        b.iter(|| {
            rt.block_on(async {
                cached_hit_service.ready().await.unwrap();
                let resp = cached_hit_service.call(hit_request.clone()).await.unwrap();
                black_box(resp.status());
            });
        });
    });

    c.bench_function("layer_throughput/cache_miss", |b| {
        b.iter(|| {
            let idx = miss_cursor.fetch_add(1, Ordering::Relaxed);
            let req = miss_requests[idx % miss_requests.len()].clone();
            rt.block_on(async {
                cached_miss_service.ready().await.unwrap();
                let resp = cached_miss_service.call(req).await.unwrap();
                black_box(resp.status());
            });
        });
    });

    c.bench_function("layer_throughput/backend_hit_count", |b| {
        b.iter(|| black_box(inner_counter.load(Ordering::Relaxed)));
    });
}

fn bench_key_extractor(c: &mut Criterion) {
    let path = KeyExtractor::path();
    let path_query = KeyExtractor::path_and_query();
    let header = KeyExtractor::custom(|method, uri| {
        if method == Method::GET && uri.path().starts_with("/assets") {
            Some(format!("static:{}", uri.path()))
        } else {
            None
        }
    });

    let method_get = Method::GET;
    let method_post = Method::POST;
    let uri = Uri::from_static("/products/123?region=us&currency=usd");

    c.bench_function("key_extractor/path", |b| {
        b.iter(|| {
            let key = path.extract(&method_get, &uri);
            black_box(key);
        });
    });

    c.bench_function("key_extractor/path_and_query", |b| {
        b.iter(|| {
            let key = path_query.extract(&method_get, &uri);
            black_box(key);
        });
    });

    c.bench_function("key_extractor/custom_hit", |b| {
        b.iter(|| {
            let key = header.extract(&method_get, &Uri::from_static("/assets/logo.svg"));
            black_box(key);
        });
    });

    c.bench_function("key_extractor/custom_miss", |b| {
        b.iter(|| {
            let key = header.extract(&method_post, &uri);
            black_box(key);
        });
    });
}

fn bench_in_memory_backend(c: &mut Criterion) {
    let rt = tokio_runtime();
    let backend = InMemoryBackend::new(10_000);
    let entry_small = sample_entry(256);
    let entry_large = sample_entry(64 * 1024);
    let ttl = Duration::from_secs(60);
    let stale = Duration::from_secs(10);
    let key = "bench:small".to_string();
    let key_large = "bench:large".to_string();

    rt.block_on(async {
        backend
            .set(key.clone(), entry_small.clone(), ttl, stale)
            .await
            .unwrap();
        backend
            .set(key_large.clone(), entry_large.clone(), ttl, stale)
            .await
            .unwrap();
    });

    c.bench_function("backend/in_memory/get_small_hit", |b| {
        b.iter(|| {
            rt.block_on(async {
                let hit = backend.get(&key).await.unwrap();
                black_box(hit);
            });
        });
    });

    c.bench_function("backend/in_memory/get_large_hit", |b| {
        b.iter(|| {
            rt.block_on(async {
                let hit = backend.get(&key_large).await.unwrap();
                black_box(hit);
            });
        });
    });

    c.bench_function("backend/in_memory/set_small", |b| {
        b.iter(|| {
            rt.block_on(async {
                backend
                    .set(key.clone(), entry_small.clone(), ttl, stale)
                    .await
                    .unwrap();
            });
        });
    });

    c.bench_function("backend/in_memory/set_large", |b| {
        b.iter(|| {
            rt.block_on(async {
                backend
                    .set(key_large.clone(), entry_large.clone(), ttl, stale)
                    .await
                    .unwrap();
            });
        });
    });
}

fn bench_stampede_protection(c: &mut Criterion) {
    let rt = tokio_runtime();
    let concurrency = 32;
    let slow_counter = Arc::new(AtomicUsize::new(0));

    let slow_backend = tower::service_fn({
        let counter = slow_counter.clone();
        move |_req: Request<()>| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::Relaxed);
                sleep(Duration::from_millis(4)).await;
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::from(Bytes::from_static(b"slow-response")))
                        .unwrap(),
                )
            }
        }
    });

    c.bench_function("stampede/cache_layer", |b| {
        b.iter(|| {
            rt.block_on(async {
                let layer = Arc::new(
                    CacheLayer::builder(InMemoryBackend::new(10_000))
                        .ttl(Duration::from_millis(50))
                        .stale_while_revalidate(Duration::from_millis(100))
                        .refresh_before(Duration::from_millis(10))
                        .build(),
                );
                let mut tasks = Vec::with_capacity(concurrency);
                for _ in 0..concurrency {
                    let layer = layer.clone();
                    let svc = slow_backend.clone();
                    let req = request("/stampede/hot");
                    tasks.push(tokio::spawn(async move {
                        let mut svc = layer.layer(svc);
                        svc.ready().await.unwrap();
                        let resp = svc.call(req).await.unwrap();
                        black_box(resp.status());
                    }));
                }
                for task in tasks {
                    task.await.unwrap();
                }
            });
        });
    });

    c.bench_function("stampede/no_cache", |b| {
        b.iter(|| {
            rt.block_on(async {
                let mut tasks = Vec::with_capacity(concurrency);
                for _ in 0..concurrency {
                    let svc = slow_backend.clone();
                    let req = request("/stampede/hot");
                    tasks.push(tokio::spawn(async move {
                        let mut svc = svc;
                        svc.ready().await.unwrap();
                        let resp = svc.call(req).await.unwrap();
                        black_box(resp.status());
                    }));
                }
                for task in tasks {
                    task.await.unwrap();
                }
            });
        });
    });

    c.bench_function("stampede/backend_calls", |b| {
        b.iter(|| {
            let calls = slow_counter.swap(0, Ordering::Relaxed);
            black_box(calls);
        });
    });
}

fn bench_stale_while_revalidate(c: &mut Criterion) {
    let rt = tokio_runtime();
    let counter = Arc::new(AtomicUsize::new(0));
    let slow_origin = tower::service_fn({
        let counter = counter.clone();
        move |_req: Request<()>| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::Relaxed);
                sleep(Duration::from_millis(5)).await;
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::from(Bytes::from_static(b"fresh-blob")))
                        .unwrap(),
                )
            }
        }
    });

    let stale_layer = CacheLayer::builder(InMemoryBackend::new(1_000))
        .ttl(Duration::from_millis(20))
        .stale_while_revalidate(Duration::from_millis(120))
        .refresh_before(Duration::from_millis(15))
        .build();

    let strict_layer = CacheLayer::builder(InMemoryBackend::new(1_000))
        .ttl(Duration::from_millis(20))
        .stale_while_revalidate(Duration::from_millis(0))
        .refresh_before(Duration::from_millis(0))
        .build();

    let bench_request = request("/stale/example");

    c.bench_function("stale_while_revalidate/stale_hit_latency", |b| {
        b.iter(|| {
            rt.block_on(async {
                let mut svc = stale_layer.layer(slow_origin.clone());
                svc.ready().await.unwrap();
                let _ = svc.call(bench_request.clone()).await.unwrap();
                sleep(Duration::from_millis(25)).await;
                let start = Instant::now();
                let mut svc = stale_layer.layer(slow_origin.clone());
                svc.ready().await.unwrap();
                let resp = svc.call(bench_request.clone()).await.unwrap();
                black_box(resp.status());
                black_box(start.elapsed());
            });
        });
    });

    c.bench_function("stale_while_revalidate/strict_refresh_latency", |b| {
        b.iter(|| {
            rt.block_on(async {
                let mut svc = strict_layer.layer(slow_origin.clone());
                svc.ready().await.unwrap();
                let _ = svc.call(bench_request.clone()).await.unwrap();
                sleep(Duration::from_millis(25)).await;
                let start = Instant::now();
                let mut svc = strict_layer.layer(slow_origin.clone());
                svc.ready().await.unwrap();
                let resp = svc.call(bench_request.clone()).await.unwrap();
                black_box(resp.status());
                black_box(start.elapsed());
            });
        });
    });
}

fn bench_codec_and_compression(c: &mut Criterion) {
    let codec = BincodeCodec::default();
    let entry_small = sample_entry(512);
    let entry_large = sample_entry(256 * 1024);

    c.bench_function("codec/bincode_encode_small", |b| {
        b.iter(|| {
            let bytes = codec.encode(black_box(&entry_small)).unwrap();
            black_box(bytes);
        });
    });

    c.bench_function("codec/bincode_decode_small", |b| {
        let encoded = codec.encode(&entry_small).unwrap();
        b.iter(|| {
            let entry = codec.decode(black_box(&encoded)).unwrap();
            black_box(entry.status);
        });
    });

    c.bench_function("codec/bincode_encode_large", |b| {
        b.iter(|| {
            let bytes = codec.encode(black_box(&entry_large)).unwrap();
            black_box(bytes.len());
        });
    });

    c.bench_function("codec/bincode_decode_large", |b| {
        let encoded = codec.encode(&entry_large).unwrap();
        b.iter(|| {
            let entry = codec.decode(black_box(&encoded)).unwrap();
            black_box(entry.body.len());
        });
    });

    #[cfg(feature = "compression")]
    {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;

        let payload = vec![b'a'; 256 * 1024];
        c.bench_function("compression/gzip_large_payload", |b| {
            b.iter(|| {
                let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
                encoder.write_all(black_box(&payload)).unwrap();
                let bytes = encoder.finish().unwrap();
                black_box(bytes.len());
            });
        });
    }
}

fn bench_negative_cache(c: &mut Criterion) {
    let rt = tokio_runtime();
    let backend_hits = Arc::new(AtomicUsize::new(0));
    let miss_service = tower::service_fn({
        let counter = backend_hits.clone();
        move |_req: Request<()>| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::Relaxed);
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body(Full::from(Bytes::from_static(b"not-found")))
                        .unwrap(),
                )
            }
        }
    });

    let stored_layer = CacheLayer::builder(InMemoryBackend::new(5_000))
        .ttl(Duration::from_secs(60))
        .negative_ttl(Duration::from_millis(80))
        .build();

    let negative_request = request("/missing/resource");
    let mut stored_service = stored_layer.layer(miss_service.clone());
    rt.block_on(async {
        stored_service.ready().await.unwrap();
        let _ = stored_service.call(negative_request.clone()).await.unwrap();
    });

    c.bench_function("negative_cache/initial_miss", |b| {
        b.iter(|| {
            rt.block_on(async {
                let mut svc = CacheLayer::builder(InMemoryBackend::new(5_000))
                    .ttl(Duration::from_secs(60))
                    .negative_ttl(Duration::from_millis(80))
                    .build()
                    .layer(miss_service.clone());
                svc.ready().await.unwrap();
                let resp = svc.call(negative_request.clone()).await.unwrap();
                black_box(resp.status());
            });
        });
    });

    c.bench_function("negative_cache/stored_negative_hit", |b| {
        b.iter(|| {
            rt.block_on(async {
                sleep(Duration::from_millis(20)).await;
                stored_service.ready().await.unwrap();
                let resp = stored_service.call(negative_request.clone()).await.unwrap();
                black_box(resp.status());
            });
        });
    });

    c.bench_function("negative_cache/after_ttl_churn", |b| {
        let unique_requests: Vec<Request<()>> = (0..256)
            .map(|i| request(&format!("/missing/resource/{i}")))
            .collect();
        let cursor = Arc::new(AtomicUsize::new(0));
        b.iter(|| {
            let idx = cursor.fetch_add(1, Ordering::Relaxed);
            let req = unique_requests[idx % unique_requests.len()].clone();
            rt.block_on(async {
                let mut svc = CacheLayer::builder(InMemoryBackend::new(5_000))
                    .ttl(Duration::from_secs(60))
                    .negative_ttl(Duration::from_millis(10))
                    .build()
                    .layer(miss_service.clone());
                svc.ready().await.unwrap();
                let resp = svc.call(req).await.unwrap();
                black_box(resp.status());
            });
        });
    });

    c.bench_function("negative_cache/backend_calls", |b| {
        b.iter(|| {
            let calls = backend_hits.load(Ordering::Relaxed);
            black_box(calls);
        });
    });
}

#[cfg(feature = "metrics")]
fn bench_instrumentation_overhead(c: &mut Criterion) {
    use metrics_exporter_null::NullBuilder;

    static RECORDER: OnceLock<()> = OnceLock::new();
    RECORDER.get_or_init(|| {
        NullBuilder::default()
            .install()
            .expect("null recorder to install");
    });

    let rt = tokio_runtime();
    let instrumented_layer = CacheLayer::builder(InMemoryBackend::new(2_000))
        .ttl(Duration::from_secs(60))
        .stale_while_revalidate(Duration::from_secs(30))
        .build();

    let instrumentation_request = request("/instrumentation/hit");
    let origin = tower::service_fn(|_req: Request<()>| async move {
        Ok::<_, Infallible>(
            Response::builder()
                .status(StatusCode::OK)
                .body(Full::from(Bytes::from_static(b"instrumented")))
                .unwrap(),
        )
    });

    let mut instrumented_service = instrumented_layer.layer(origin.clone());
    let mut raw_service = origin;

    c.bench_function("instrumentation/with_metrics", |b| {
        b.iter(|| {
            rt.block_on(async {
                instrumented_service.ready().await.unwrap();
                let resp = instrumented_service
                    .call(instrumentation_request.clone())
                    .await
                    .unwrap();
                black_box(resp.status());
            });
        });
    });

    c.bench_function("instrumentation/no_metrics", |b| {
        b.iter(|| {
            rt.block_on(async {
                raw_service.ready().await.unwrap();
                let resp = raw_service
                    .call(instrumentation_request.clone())
                    .await
                    .unwrap();
                black_box(resp.status());
            });
        });
    });
}

#[cfg(feature = "metrics")]
fn instrumentation_entry(c: &mut Criterion) {
    bench_instrumentation_overhead(c);
}

#[cfg(not(feature = "metrics"))]
fn instrumentation_entry(_c: &mut Criterion) {}

fn init_criterion() -> Criterion {
    Criterion::default().sample_size(50)
}

criterion_group! {
    name = benches;
    config = init_criterion();
    targets =
        bench_layer_throughput,
        bench_key_extractor,
        bench_in_memory_backend,
        bench_stampede_protection,
        bench_stale_while_revalidate,
        bench_codec_and_compression,
        bench_negative_cache,
        instrumentation_entry
}

criterion_main!(benches);
