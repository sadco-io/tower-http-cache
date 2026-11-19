//! Isolated integration test for ML logging bypass events.
//!
//! Kept in its own test binary so `tracing::subscriber::set_default` never
//! contends with tracing callsites executed concurrently by unrelated tests.

use std::convert::Infallible;
use std::io::Write;
use std::sync::{Arc, Mutex};

use http::Request;
use http_body_util::Full;
use tower::{Layer, Service, ServiceExt, service_fn};
use tower_http_cache::logging::MLLoggingConfig;
use tower_http_cache::policy::CachePolicy;
use tower_http_cache::prelude::*;

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

#[tokio::test]
async fn ml_logging_bypass_emits_single_bypass_event_and_hashes_uri() {
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
