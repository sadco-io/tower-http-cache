use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use http::Request;
use http::Response;
use http_body_util::{BodyExt, Full};
use tokio::time::sleep;
use tower::service_fn;
use tower::{Layer, Service, ServiceExt};
use tower_http_cache::backend::memory::InMemoryBackend;
use tower_http_cache::prelude::*;

#[tokio::test]
async fn refreshes_after_expiry_with_stale_window() {
    let counter = Arc::new(AtomicUsize::new(0));

    let backend = InMemoryBackend::new(32);
    let layer = CacheLayer::builder(backend)
        .ttl(Duration::from_millis(100))
        .stale_while_revalidate(Duration::from_secs(1))
        .refresh_before(Duration::from_millis(50))
        .build();

    let mut service = layer.layer(service_fn({
        let counter = counter.clone();
        move |_req: Request<()>| {
            let counter = counter.clone();
            async move {
                let count = counter.fetch_add(1, Ordering::SeqCst) + 1;
                let body = Full::from(count.to_string());
                Ok::<_, std::convert::Infallible>(Response::new(body))
            }
        }
    }));

    let body = service
        .ready()
        .await
        .unwrap()
        .call(Request::new(()))
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(body, "1");

    sleep(Duration::from_millis(150)).await;

    let body = service
        .ready()
        .await
        .unwrap()
        .call(Request::new(()))
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(body, "2");

    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn min_body_size_skip_prevents_caching() {
    let backend = InMemoryBackend::new(32);
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
                let idx = counter.fetch_add(1, Ordering::SeqCst) + 1;
                let body = Full::from(format!("stub-{idx}"));
                Ok::<_, std::convert::Infallible>(Response::new(body))
            }
        }
    }));

    let first = service
        .ready()
        .await
        .unwrap()
        .call(Request::new(()))
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(first, "stub-1");

    let second = service
        .ready()
        .await
        .unwrap()
        .call(Request::new(()))
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(second, "stub-2");

    assert_eq!(counter.load(Ordering::SeqCst), 2);
}
