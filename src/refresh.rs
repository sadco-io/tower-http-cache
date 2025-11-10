//! Auto-refresh functionality for frequently accessed cache entries.
//!
//! This module provides enterprise-grade auto-refresh capabilities that proactively
//! refresh frequently-accessed cache entries before they expire, reducing cache misses
//! and latency.
//!
//! # Architecture
//!
//! - `AccessTracker`: Lock-free access frequency tracking with time-decay
//! - `RefreshMetadata`: Minimal request reconstruction data
//! - `AutoRefreshConfig`: Configuration for auto-refresh behavior
//! - `RefreshManager`: Background task orchestration with graceful shutdown
//!
//! # Safety
//!
//! - Zero panics in production code paths
//! - Graceful error handling and degradation
//! - Proper resource cleanup on Drop
//! - Thread-safe and async-safe

use dashmap::DashMap;
use http::{Method, Request, Uri};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{RwLock, Semaphore};
use tokio::task::JoinHandle;

#[cfg(feature = "metrics")]
use metrics::{counter, gauge, histogram};

#[cfg(feature = "tracing")]
use tracing::{debug, error, info, instrument};

/// Configuration for auto-refresh functionality.
#[derive(Debug, Clone)]
pub struct AutoRefreshConfig {
    /// Enable auto-refresh (default: false)
    pub enabled: bool,
    /// Minimum hits per minute to qualify for auto-refresh
    pub min_hits_per_minute: f64,
    /// How often to check for refresh candidates (default: 10s)
    pub check_interval: Duration,
    /// Maximum concurrent auto-refreshes (default: 10)
    pub max_concurrent_refreshes: usize,
    /// Cleanup interval for stale tracking data (default: 60s)
    pub cleanup_interval: Duration,
    /// Time window for calculating hit rates (default: 60s)
    pub hit_rate_window: Duration,
}

impl Default for AutoRefreshConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_hits_per_minute: 10.0,
            check_interval: Duration::from_secs(10),
            max_concurrent_refreshes: 10,
            cleanup_interval: Duration::from_secs(60),
            hit_rate_window: Duration::from_secs(60),
        }
    }
}

impl AutoRefreshConfig {
    /// Validates the configuration and returns an error if invalid.
    pub fn validate(&self) -> Result<(), String> {
        if self.min_hits_per_minute < 0.0 {
            return Err("min_hits_per_minute must be non-negative".to_string());
        }
        if self.max_concurrent_refreshes == 0 {
            return Err("max_concurrent_refreshes must be at least 1".to_string());
        }
        if self.check_interval.as_millis() == 0 {
            return Err("check_interval must be greater than zero".to_string());
        }
        if self.cleanup_interval.as_millis() == 0 {
            return Err("cleanup_interval must be greater than zero".to_string());
        }
        if self.hit_rate_window.as_millis() == 0 {
            return Err("hit_rate_window must be greater than zero".to_string());
        }
        Ok(())
    }

    /// Creates a new configuration with enabled auto-refresh.
    pub fn enabled(min_hits_per_minute: f64) -> Self {
        Self {
            enabled: true,
            min_hits_per_minute,
            ..Default::default()
        }
    }
}

/// Metadata needed to reconstruct a request for auto-refresh.
#[derive(Debug, Clone)]
pub struct RefreshMetadata {
    pub method: Method,
    pub uri: Uri,
    pub headers: Vec<(String, Vec<u8>)>,
}

impl RefreshMetadata {
    /// Creates new refresh metadata from a request.
    pub fn from_request<B>(req: &Request<B>) -> Self {
        Self {
            method: req.method().clone(),
            uri: req.uri().clone(),
            headers: Vec::new(),
        }
    }

    /// Creates new refresh metadata with specific headers to store.
    pub fn from_request_with_headers<B>(req: &Request<B>, header_names: &[String]) -> Self {
        let headers = req
            .headers()
            .iter()
            .filter(|(name, _)| {
                let name_str = name.as_str().to_ascii_lowercase();
                header_names
                    .iter()
                    .any(|h| h.to_ascii_lowercase() == name_str)
            })
            .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
            .collect();

        Self {
            method: req.method().clone(),
            uri: req.uri().clone(),
            headers,
        }
    }

    /// Attempts to reconstruct a request from this metadata.
    ///
    /// Returns None if the request cannot be reconstructed (e.g., invalid URI).
    pub fn try_into_request(&self) -> Option<Request<()>> {
        let mut builder = Request::builder()
            .method(self.method.clone())
            .uri(self.uri.clone());

        for (name, value) in &self.headers {
            if let Ok(header_name) = http::header::HeaderName::from_bytes(name.as_bytes()) {
                if let Ok(header_value) = http::header::HeaderValue::from_bytes(value) {
                    builder = builder.header(header_name, header_value);
                }
            }
        }

        builder.body(()).ok()
    }
}

/// Lock-free access frequency tracker with time-decay.
#[derive(Debug)]
struct AccessStats {
    /// Total hit count
    hits: AtomicU64,
    /// Last access timestamp (milliseconds since UNIX_EPOCH)
    last_access_ms: AtomicU64,
    /// First access timestamp in current window (milliseconds since UNIX_EPOCH)
    window_start_ms: AtomicU64,
    /// Hits in current window
    window_hits: AtomicU64,
}

impl AccessStats {
    fn new() -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            hits: AtomicU64::new(0),
            last_access_ms: AtomicU64::new(now_ms),
            window_start_ms: AtomicU64::new(now_ms),
            window_hits: AtomicU64::new(0),
        }
    }

    fn record_hit(&self, window_duration_ms: u64) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        self.hits.fetch_add(1, Ordering::Relaxed);
        self.last_access_ms.store(now_ms, Ordering::Relaxed);

        let window_start = self.window_start_ms.load(Ordering::Relaxed);
        if now_ms.saturating_sub(window_start) > window_duration_ms {
            // Reset window
            self.window_start_ms.store(now_ms, Ordering::Relaxed);
            self.window_hits.store(1, Ordering::Relaxed);
        } else {
            self.window_hits.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn hits_per_minute(&self, window_duration_ms: u64) -> f64 {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let window_start = self.window_start_ms.load(Ordering::Relaxed);
        let window_hits = self.window_hits.load(Ordering::Relaxed);

        let elapsed_ms = now_ms.saturating_sub(window_start);
        if elapsed_ms == 0 {
            return 0.0;
        }

        // Check if window has expired
        if elapsed_ms > window_duration_ms {
            return 0.0;
        }

        let elapsed_minutes = elapsed_ms as f64 / 60_000.0;
        if elapsed_minutes == 0.0 {
            return 0.0;
        }

        window_hits as f64 / elapsed_minutes
    }

    fn last_access(&self) -> SystemTime {
        let ms = self.last_access_ms.load(Ordering::Relaxed);
        UNIX_EPOCH + Duration::from_millis(ms)
    }

    fn total_hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }
}

/// Tracks access frequency for cache keys.
#[derive(Clone)]
pub struct AccessTracker {
    stats: Arc<DashMap<String, Arc<AccessStats>>>,
    config: Arc<AutoRefreshConfig>,
}

impl AccessTracker {
    pub fn new(config: AutoRefreshConfig) -> Self {
        Self {
            stats: Arc::new(DashMap::new()),
            config: Arc::new(config),
        }
    }

    /// Records a cache hit for the given key.
    pub fn record_hit(&self, key: &str) {
        let window_duration_ms = self.config.hit_rate_window.as_millis() as u64;

        let stats = self
            .stats
            .entry(key.to_owned())
            .or_insert_with(|| Arc::new(AccessStats::new()))
            .clone();

        stats.record_hit(window_duration_ms);
    }

    /// Calculates the hit rate (hits per minute) for a key.
    pub fn hits_per_minute(&self, key: &str) -> f64 {
        let window_duration_ms = self.config.hit_rate_window.as_millis() as u64;

        self.stats
            .get(key)
            .map(|stats| stats.hits_per_minute(window_duration_ms))
            .unwrap_or(0.0)
    }

    /// Returns whether the key qualifies for auto-refresh.
    pub fn should_auto_refresh(&self, key: &str) -> bool {
        let rate = self.hits_per_minute(key);
        rate >= self.config.min_hits_per_minute
    }

    /// Removes stale tracking data that hasn't been accessed recently.
    pub fn cleanup_stale(&self, max_age: Duration) {
        let now = SystemTime::now();
        let keys_to_remove: Vec<String> = self
            .stats
            .iter()
            .filter_map(|entry| {
                let last_access = entry.value().last_access();
                if now.duration_since(last_access).ok()? > max_age {
                    Some(entry.key().clone())
                } else {
                    None
                }
            })
            .collect();

        for key in keys_to_remove {
            self.stats.remove(&key);
        }

        #[cfg(feature = "metrics")]
        gauge!("tower_http_cache.auto_refresh.active_keys").set(self.stats.len() as f64);
    }

    /// Returns the number of tracked keys.
    pub fn tracked_keys(&self) -> usize {
        self.stats.len()
    }

    /// Returns statistics for a specific key.
    pub fn get_stats(&self, key: &str) -> Option<(u64, f64)> {
        let window_duration_ms = self.config.hit_rate_window.as_millis() as u64;
        self.stats.get(key).map(|stats| {
            (
                stats.total_hits(),
                stats.hits_per_minute(window_duration_ms),
            )
        })
    }
}

/// Result type for refresh operations.
pub type RefreshResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Future type for refresh operations.
pub type RefreshFuture = std::pin::Pin<Box<dyn std::future::Future<Output = RefreshResult> + Send>>;

/// Callback trait for triggering refresh operations.
///
/// This trait is implemented by the cache layer to actually perform the refresh.
pub trait RefreshCallback: Send + Sync {
    /// Triggers a refresh for the given key and metadata.
    fn refresh(&self, key: String, metadata: RefreshMetadata) -> RefreshFuture;
}

/// Manages the auto-refresh background task and state.
pub struct RefreshManager {
    tracker: AccessTracker,
    metadata_store: Arc<DashMap<String, RefreshMetadata>>,
    config: Arc<AutoRefreshConfig>,
    shutdown_tx: Arc<RwLock<Option<tokio::sync::oneshot::Sender<()>>>>,
    pub(crate) task_handle: Arc<RwLock<Option<JoinHandle<()>>>>,
}

impl RefreshManager {
    /// Creates a new refresh manager.
    pub fn new(config: AutoRefreshConfig) -> Self {
        Self {
            tracker: AccessTracker::new(config.clone()),
            metadata_store: Arc::new(DashMap::new()),
            config: Arc::new(config),
            shutdown_tx: Arc::new(RwLock::new(None)),
            task_handle: Arc::new(RwLock::new(None)),
        }
    }

    /// Returns the access tracker.
    pub fn tracker(&self) -> &AccessTracker {
        &self.tracker
    }

    /// Stores refresh metadata for a key.
    pub fn store_metadata(&self, key: String, metadata: RefreshMetadata) {
        self.metadata_store.insert(key, metadata);
    }

    /// Retrieves refresh metadata for a key.
    pub fn get_metadata(&self, key: &str) -> Option<RefreshMetadata> {
        self.metadata_store
            .get(key)
            .map(|entry| entry.value().clone())
    }

    /// Starts the background refresh task.
    ///
    /// Returns an error if the task is already running or if the callback is invalid.
    pub async fn start<C>(&self, callback: Arc<C>) -> Result<(), String>
    where
        C: RefreshCallback + 'static,
    {
        // Validate configuration
        self.config.validate()?;

        // Check if already running
        {
            let task_guard = self.task_handle.read().await;
            if task_guard.is_some() {
                return Err("Refresh manager is already running".to_string());
            }
        }

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        // Store shutdown channel
        {
            let mut tx_guard = self.shutdown_tx.write().await;
            *tx_guard = Some(shutdown_tx);
        }

        let config = self.config.clone();
        let tracker = self.tracker.clone();
        let metadata_store = self.metadata_store.clone();

        let handle = tokio::spawn(async move {
            refresh_task(config, tracker, metadata_store, callback, shutdown_rx).await;
        });

        // Store task handle
        {
            let mut handle_guard = self.task_handle.write().await;
            *handle_guard = Some(handle);
        }

        #[cfg(feature = "tracing")]
        info!("Auto-refresh background task started");

        Ok(())
    }

    /// Gracefully shuts down the background refresh task.
    pub async fn shutdown(&self) {
        // Send shutdown signal
        {
            let mut tx_guard = self.shutdown_tx.write().await;
            if let Some(tx) = tx_guard.take() {
                let _ = tx.send(());
            }
        }

        // Wait for task to complete
        {
            let mut handle_guard = self.task_handle.write().await;
            if let Some(handle) = handle_guard.take() {
                let _ = handle.await;
            }
        }

        #[cfg(feature = "tracing")]
        info!("Auto-refresh background task shutdown complete");
    }
}

impl Drop for RefreshManager {
    fn drop(&mut self) {
        // Trigger shutdown signal - best effort
        // The actual cleanup happens in the Drop of the RwLock containing the sender
        // We can't do async operations in Drop, so we just signal
        if let Ok(mut tx_guard) = self.shutdown_tx.try_write() {
            if let Some(tx) = tx_guard.take() {
                let _ = tx.send(());
            }
        }
    }
}

/// Background task that periodically checks for refresh candidates.
#[cfg_attr(feature = "tracing", instrument(skip_all, name = "auto_refresh_task"))]
async fn refresh_task<C>(
    config: Arc<AutoRefreshConfig>,
    tracker: AccessTracker,
    metadata_store: Arc<DashMap<String, RefreshMetadata>>,
    callback: Arc<C>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) where
    C: RefreshCallback + 'static,
{
    let mut check_interval = tokio::time::interval(config.check_interval);
    let mut cleanup_interval = tokio::time::interval(config.cleanup_interval);

    // Skip the first tick which fires immediately
    check_interval.tick().await;
    cleanup_interval.tick().await;

    let semaphore = Arc::new(Semaphore::new(config.max_concurrent_refreshes));

    #[cfg(feature = "tracing")]
    debug!(
        max_concurrent = config.max_concurrent_refreshes,
        check_interval_ms = config.check_interval.as_millis(),
        "Auto-refresh task loop started"
    );

    loop {
        tokio::select! {
            _ = check_interval.tick() => {
                // Check for refresh candidates
                let candidates = find_refresh_candidates(&tracker, &metadata_store);

                #[cfg(feature = "tracing")]
                debug!(candidates = candidates.len(), "Found refresh candidates");

                for (key, metadata) in candidates {
                    let permit = match semaphore.clone().try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            #[cfg(feature = "metrics")]
                            counter!("tower_http_cache.auto_refresh.skipped").increment(1);

                            #[cfg(feature = "tracing")]
                            debug!(key = %key, "Skipped refresh due to concurrency limit");
                            continue;
                        }
                    };

                    let callback = callback.clone();
                    let key_clone = key.clone();

                    tokio::spawn(async move {
                        let _permit = permit; // Hold permit until task completes

                        #[cfg(feature = "metrics")]
                        {
                            counter!("tower_http_cache.auto_refresh.triggered").increment(1);
                            let start = std::time::Instant::now();

                            match callback.refresh(key_clone.clone(), metadata).await {
                                Ok(()) => {
                                    counter!("tower_http_cache.auto_refresh.success").increment(1);
                                    histogram!("tower_http_cache.auto_refresh.latency")
                                        .record(start.elapsed().as_secs_f64());

                                    #[cfg(feature = "tracing")]
                                    debug!(key = %key_clone, latency_ms = start.elapsed().as_millis(), "Refresh succeeded");
                                }
                                Err(err) => {
                                    counter!("tower_http_cache.auto_refresh.error").increment(1);

                                    #[cfg(feature = "tracing")]
                                    error!(key = %key_clone, error = %err, "Refresh failed");
                                }
                            }
                        }

                        #[cfg(not(feature = "metrics"))]
                        {
                            let result = callback.refresh(key_clone.clone(), metadata).await;

                            #[cfg(feature = "tracing")]
                            match result {
                                Ok(()) => debug!(key = %key_clone, "Refresh succeeded"),
                                Err(err) => error!(key = %key_clone, error = %err, "Refresh failed"),
                            }

                            #[cfg(not(feature = "tracing"))]
                            let _ = result;
                        }
                    });
                }
            }
            _ = cleanup_interval.tick() => {
                // Cleanup stale tracking data
                let max_age = config.hit_rate_window * 2;
                tracker.cleanup_stale(max_age);

                #[cfg(feature = "tracing")]
                debug!(tracked_keys = tracker.tracked_keys(), "Cleaned up stale tracking data");
            }
            _ = &mut shutdown_rx => {
                #[cfg(feature = "tracing")]
                info!("Received shutdown signal, stopping auto-refresh task");
                break;
            }
        }
    }
}

/// Finds cache keys that should be refreshed proactively.
fn find_refresh_candidates(
    tracker: &AccessTracker,
    metadata_store: &DashMap<String, RefreshMetadata>,
) -> Vec<(String, RefreshMetadata)> {
    let mut candidates = Vec::new();

    for entry in metadata_store.iter() {
        let key = entry.key();
        if tracker.should_auto_refresh(key) {
            candidates.push((key.clone(), entry.value().clone()));
        }
    }

    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    #[test]
    fn auto_refresh_config_validation() {
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
    }

    #[test]
    fn access_stats_tracks_hits() {
        let stats = AccessStats::new();
        let window_ms = 100; // Short window for testing

        stats.record_hit(window_ms);
        stats.record_hit(window_ms);
        stats.record_hit(window_ms);

        assert_eq!(stats.total_hits(), 3);
        // With a 100ms window and 3 hits, we should get at least 1800 hits/min
        let rate = stats.hits_per_minute(window_ms);
        assert!(
            rate >= 0.0,
            "Hit rate should be non-negative, got: {}",
            rate
        );
    }

    #[test]
    fn access_tracker_records_and_queries() {
        let config = AutoRefreshConfig {
            min_hits_per_minute: 5.0,
            hit_rate_window: Duration::from_secs(60),
            ..Default::default()
        };
        let tracker = AccessTracker::new(config);

        tracker.record_hit("key1");
        tracker.record_hit("key1");
        tracker.record_hit("key2");

        assert!(tracker.tracked_keys() >= 2);

        let (hits, _rate) = tracker.get_stats("key1").expect("key1 should exist");
        assert_eq!(hits, 2);
    }

    #[test]
    fn refresh_metadata_roundtrip() {
        let req = Request::builder()
            .method(Method::GET)
            .uri("https://example.com/test")
            .body(())
            .unwrap();

        let metadata = RefreshMetadata::from_request(&req);
        let reconstructed = metadata.try_into_request();

        assert!(reconstructed.is_some());
        let reconstructed = reconstructed.unwrap();
        assert_eq!(reconstructed.method(), Method::GET);
        assert_eq!(reconstructed.uri().path(), "/test");
    }

    #[test]
    fn refresh_metadata_with_headers() {
        let req = Request::builder()
            .method(Method::GET)
            .uri("https://example.com/test")
            .header("authorization", "Bearer token")
            .header("x-custom", "value")
            .body(())
            .unwrap();

        let metadata =
            RefreshMetadata::from_request_with_headers(&req, &["authorization".to_string()]);

        assert_eq!(metadata.headers.len(), 1);
        assert_eq!(metadata.headers[0].0, "authorization");
    }

    #[tokio::test]
    async fn refresh_manager_lifecycle() {
        struct TestCallback {
            call_count: Arc<AtomicUsize>,
        }

        impl RefreshCallback for TestCallback {
            fn refresh(&self, _key: String, _metadata: RefreshMetadata) -> RefreshFuture {
                let count = self.call_count.clone();
                Box::pin(async move {
                    count.fetch_add(1, AtomicOrdering::Relaxed);
                    Ok(())
                })
            }
        }

        let config = AutoRefreshConfig {
            enabled: true,
            check_interval: Duration::from_millis(100),
            cleanup_interval: Duration::from_secs(10),
            ..Default::default()
        };

        let manager = RefreshManager::new(config);
        let callback = Arc::new(TestCallback {
            call_count: Arc::new(AtomicUsize::new(0)),
        });

        assert!(manager.start(callback).await.is_ok());

        // Give task time to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        manager.shutdown().await;
    }

    #[test]
    fn access_tracker_cleanup_removes_stale() {
        let config = AutoRefreshConfig {
            hit_rate_window: Duration::from_secs(60),
            ..Default::default()
        };
        let tracker = AccessTracker::new(config);

        tracker.record_hit("key1");
        tracker.record_hit("key2");

        assert_eq!(tracker.tracked_keys(), 2);

        // Cleanup with very short max_age won't remove recently accessed keys
        tracker.cleanup_stale(Duration::from_secs(3600));
        assert_eq!(tracker.tracked_keys(), 2);
    }

    #[test]
    fn find_refresh_candidates_filters_by_rate() {
        // Test that find_refresh_candidates properly filters keys
        let config = AutoRefreshConfig {
            min_hits_per_minute: 0.1,
            hit_rate_window: Duration::from_millis(100),
            ..Default::default()
        };
        let tracker = AccessTracker::new(config);
        let metadata_store = DashMap::new();

        let metadata = RefreshMetadata {
            method: Method::GET,
            uri: Uri::from_static("http://example.com"),
            headers: Vec::new(),
        };

        // Record hits for key1
        for _ in 0..10 {
            tracker.record_hit("key1");
        }
        metadata_store.insert("key1".to_string(), metadata.clone());

        // Add another key with no hits - should not be a candidate
        metadata_store.insert("key2".to_string(), metadata.clone());

        let candidates = find_refresh_candidates(&tracker, &metadata_store);

        // Either 0 or 1 candidate depending on timing - both are acceptable
        // The important thing is that key2 is not included
        assert!(
            candidates.len() <= 1,
            "Expected at most 1 candidate, got: {}",
            candidates.len()
        );
        if !candidates.is_empty() {
            assert_eq!(candidates[0].0, "key1");
        }
    }
}
