use std::error::Error as StdError;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use http::header::{CACHE_CONTROL, PRAGMA};
use http::{HeaderMap, Method, Request, Response, Uri};
use http_body::Body;
use http_body_util::combinators::{BoxBody, UnsyncBoxBody};
use http_body_util::{BodyExt, Full};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tower::{Layer, Service, ServiceExt};

#[cfg(feature = "metrics")]
use metrics::{counter, histogram};

#[cfg(feature = "admin-api")]
use crate::admin::stats::GlobalStats;
#[cfg(feature = "in-memory")]
use crate::backend::memory::InMemoryBackend;
use crate::backend::{CacheBackend, CacheEntry, CacheRead};
use crate::chunks::{ChunkCache, ChunkMetadata, ChunkedEntry};
#[cfg(feature = "serde")]
use crate::logging::{CacheEvent, CacheEventType, MLLoggingConfig};
#[cfg(feature = "compression")]
use crate::policy::CompressionStrategy;
use crate::policy::{CachePolicy, CompressionConfig, CookiePolicy};
use crate::range::{RangeHandling, is_partial_content, parse_range_header};
use crate::refresh::{AutoRefreshConfig, RefreshCallback, RefreshManager, RefreshMetadata};
#[cfg(feature = "serde")]
use crate::request_id::RequestId;
#[cfg(feature = "tracing")]
use crate::streaming::extract_size_info;
use crate::streaming::{StreamingDecision, should_stream};

pub type BoxError = Box<dyn StdError + Send + Sync>;

/// Boxed, `Send` future used as `CacheService::Future`.
///
/// Structurally identical to `futures_util::future::BoxFuture<'static, T>`,
/// which this replaced -- keeping `futures-util` out of the runtime
/// dependency tree costs one type alias.
type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Response body type that implements [`Sync`] for Axum and Tower compatibility,
/// including when wrapping `!Sync` upstream response bodies such as `axum::body::Body`.
pub struct SyncBoxBody {
    inner: std::sync::Mutex<UnsyncBoxBody<Bytes, BoxError>>,
}

impl SyncBoxBody {
    /// Creates a new [`SyncBoxBody`] by wrapping a [`BoxBody`].
    pub fn new(inner: BoxBody<Bytes, BoxError>) -> Self {
        Self::from_body(inner)
    }

    /// Wraps any `Send + 'static` HTTP body (including `!Sync` bodies) in a [`Sync`] wrapper.
    pub fn from_body<B>(body: B) -> Self
    where
        B: Body<Data = Bytes> + Send + 'static,
        B::Error: Into<BoxError>,
    {
        Self {
            inner: std::sync::Mutex::new(body.map_err(Into::into).boxed_unsync()),
        }
    }
}

impl Body for SyncBoxBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let mut inner = self
            .get_mut()
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        Pin::new(&mut *inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .size_hint()
    }
}

/// Type alias for the key extractor function
type KeyExtractorFn = Arc<dyn Fn(&Method, &Uri) -> Option<String> + Send + Sync>;

/// Configurable caching layer for Tower services.
///
/// The layer wraps an inner service and caches HTTP responses based on the
/// configured [`CachePolicy`]. Create instances via [`CacheLayer::builder`]
/// or [`CacheLayer::new`] for a sensible default policy.
///
/// Cloning a `CacheLayer` is cheap and shares the underlying backend and
/// in-flight stampede locks.
#[derive(Clone)]
pub struct CacheLayer<B> {
    backend: B,
    policy: CachePolicy,
    key_extractor: KeyExtractor,
    locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
    refresh_manager: Option<Arc<RefreshManager>>,
    refresh_shutdown: Option<Arc<RefreshShutdownGuard>>,
    chunk_cache: Option<Arc<ChunkCache>>,
    #[cfg(feature = "admin-api")]
    stats: Arc<GlobalStats>,
}

/// Drops the background auto-refresh task only once the last [`CacheLayer`]
/// and every cloned [`CacheService`] have been dropped.
struct RefreshShutdownGuard {
    manager: Arc<RefreshManager>,
}

impl RefreshShutdownGuard {
    fn new(manager: Arc<RefreshManager>) -> Self {
        Self { manager }
    }
}

impl Drop for RefreshShutdownGuard {
    fn drop(&mut self) {
        let manager = Arc::clone(&self.manager);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                manager.shutdown().await;
            });
        }
    }
}

/// Strategy used to turn requests into cache keys.
///
/// The layer ships with helpers for common patterns such as
/// [`KeyExtractor::path_and_query`] and [`KeyExtractor::path`].
/// You can also provide your own extractor with [`KeyExtractor::custom`].
#[derive(Clone)]
pub struct KeyExtractor {
    inner: KeyExtractorFn,
}

impl KeyExtractor {
    /// Builds an extractor that uses `path + query` (omitting method) for GET/HEAD requests.
    pub fn path_and_query() -> Self {
        Self {
            inner: Arc::new(|method: &Method, uri: &Uri| {
                if matches!(method, &Method::GET | &Method::HEAD) {
                    let mut key = uri.path().to_owned();
                    if let Some(query) = uri.query() {
                        key.push('?');
                        key.push_str(query);
                    }
                    Some(key)
                } else {
                    None
                }
            }),
        }
    }

    /// Builds an extractor that uses `path` (omitting method and query) for GET/HEAD requests.
    pub fn path() -> Self {
        Self {
            inner: Arc::new(|method: &Method, uri: &Uri| {
                if matches!(method, &Method::GET | &Method::HEAD) {
                    Some(uri.path().to_owned())
                } else {
                    None
                }
            }),
        }
    }

    pub fn custom<F>(func: F) -> Self
    where
        F: Fn(&Method, &Uri) -> Option<String> + Send + Sync + 'static,
    {
        Self {
            inner: Arc::new(func),
        }
    }

    /// Extracts a cache key from the provided request parts.
    ///
    /// Returns `None` when the request should be skipped.
    pub fn extract(&self, method: &Method, uri: &Uri) -> Option<String> {
        (self.inner)(method, uri)
    }
}

impl Default for KeyExtractor {
    fn default() -> Self {
        Self::path_and_query()
    }
}

/// Builder for configuring [`CacheLayer`] instances.
pub struct CacheLayerBuilder<B> {
    backend: B,
    policy: CachePolicy,
    key_extractor: KeyExtractor,
    auto_refresh_config: Option<AutoRefreshConfig>,
    #[cfg(feature = "admin-api")]
    stats: Option<Arc<GlobalStats>>,
}

impl<B> CacheLayerBuilder<B>
where
    B: CacheBackend,
{
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            policy: CachePolicy::default(),
            key_extractor: KeyExtractor::default(),
            auto_refresh_config: None,
            #[cfg(feature = "admin-api")]
            stats: None,
        }
    }

    /// Replaces the cache policy with a pre-built value.
    pub fn policy(mut self, policy: CachePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Sets the positive cache TTL for successful responses.
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.policy = self.policy.with_ttl(ttl);
        self
    }

    /// Sets the cache TTL for negative (4xx) responses.
    pub fn negative_ttl(mut self, ttl: Duration) -> Self {
        self.policy = self.policy.with_negative_ttl(ttl);
        self
    }

    pub fn stale_while_revalidate(mut self, duration: Duration) -> Self {
        self.policy = self.policy.with_stale_while_revalidate(duration);
        self
    }

    pub fn refresh_before(mut self, duration: Duration) -> Self {
        self.policy = self.policy.with_refresh_before(duration);
        self
    }

    pub fn max_body_size(mut self, size: Option<usize>) -> Self {
        self.policy = self.policy.with_max_body_size(size);
        self
    }

    pub fn min_body_size(mut self, size: Option<usize>) -> Self {
        self.policy = self.policy.with_min_body_size(size);
        self
    }

    pub fn allow_streaming_bodies(mut self, allow: bool) -> Self {
        self.policy = self.policy.with_allow_streaming_bodies(allow);
        self
    }

    pub fn compression(mut self, config: CompressionConfig) -> Self {
        self.policy = self.policy.with_compression(config);
        self
    }

    pub fn respect_cache_control(mut self, enabled: bool) -> Self {
        self.policy = self.policy.with_respect_cache_control(enabled);
        self
    }

    pub fn statuses(mut self, statuses: impl IntoIterator<Item = u16>) -> Self {
        self.policy = self.policy.with_statuses(statuses);
        self
    }

    pub fn method_predicate<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&Method) -> bool + Send + Sync + 'static,
    {
        self.policy = self.policy.with_method_predicate(predicate);
        self
    }

    pub fn header_allowlist<I, S>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.policy = self.policy.with_header_allowlist(headers);
        self
    }

    /// Configures how requests carrying a `Cookie` header interact with the cache.
    pub fn cookie_policy(mut self, cookie_policy: CookiePolicy) -> Self {
        self.policy = self.policy.with_cookie_policy(cookie_policy);
        self
    }

    pub fn key_extractor(mut self, extractor: KeyExtractor) -> Self {
        self.key_extractor = extractor;
        self
    }

    /// Enables auto-refresh functionality with the provided configuration.
    ///
    /// When enabled, frequently accessed cache entries will be proactively
    /// refreshed before they expire, reducing cache misses and latency.
    ///
    /// Note that proactive refresh before expiry requires a non-zero
    /// [`refresh_before`](Self::refresh_before) window on the policy (the
    /// default is `Duration::ZERO`, which only qualifies already-expired
    /// entries).
    pub fn auto_refresh(mut self, config: AutoRefreshConfig) -> Self {
        self.auto_refresh_config = Some(config);
        self
    }

    /// Attaches a shared [`GlobalStats`] tracker so cache hits, misses,
    /// stores, and invalidations are surfaced through the admin API.
    #[cfg(feature = "admin-api")]
    pub fn stats(mut self, stats: Arc<GlobalStats>) -> Self {
        self.stats = Some(stats);
        self
    }

    pub fn build(self) -> CacheLayer<B> {
        let refresh_before = self.policy.refresh_before();
        let refresh_manager = self
            .auto_refresh_config
            .filter(|cfg| cfg.enabled)
            .map(|cfg| {
                #[cfg(feature = "tracing")]
                if refresh_before.is_zero() {
                    tracing::warn!(
                        "auto_refresh is enabled with refresh_before == Duration::ZERO; entries will only qualify for refresh after expiry"
                    );
                }
                Arc::new(RefreshManager::new(cfg).with_refresh_before(refresh_before))
            });
        let refresh_shutdown = refresh_manager
            .as_ref()
            .map(|manager| Arc::new(RefreshShutdownGuard::new(Arc::clone(manager))));

        // Create chunk cache if enabled in streaming policy
        let streaming_policy = self.policy.streaming_policy();
        let chunk_cache = if streaming_policy.enable_chunk_cache {
            Some(Arc::new(ChunkCache::with_limits(
                streaming_policy.chunk_size,
                streaming_policy.max_chunk_cache_bytes,
                streaming_policy.max_chunk_cache_entries,
            )))
        } else {
            None
        };

        CacheLayer {
            backend: self.backend,
            policy: self.policy,
            key_extractor: self.key_extractor,
            locks: Arc::new(DashMap::new()),
            refresh_manager,
            refresh_shutdown,
            chunk_cache,
            #[cfg(feature = "admin-api")]
            stats: self.stats.unwrap_or_else(|| Arc::new(GlobalStats::new())),
        }
    }
}

#[cfg(feature = "in-memory")]
impl CacheLayer<InMemoryBackend> {
    /// Creates a cache layer backed by an in-memory [`InMemoryBackend`].
    pub fn new_in_memory(max_capacity: u64) -> Self {
        CacheLayerBuilder::new(InMemoryBackend::new(max_capacity)).build()
    }
}

impl<B> CacheLayer<B>
where
    B: CacheBackend,
{
    /// Builds a cache layer with the default [`CachePolicy`].
    pub fn new(backend: B) -> Self {
        CacheLayerBuilder::new(backend).build()
    }

    /// Returns a builder for fine-grained control over the cache policy.
    pub fn builder(backend: B) -> CacheLayerBuilder<B> {
        CacheLayerBuilder::new(backend)
    }

    pub fn with_policy(mut self, policy: CachePolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.policy = self.policy.clone().with_ttl(ttl);
        self
    }

    pub fn with_negative_ttl(mut self, ttl: Duration) -> Self {
        self.policy = self.policy.clone().with_negative_ttl(ttl);
        self
    }

    pub fn with_stale_while_revalidate(mut self, duration: Duration) -> Self {
        self.policy = self.policy.clone().with_stale_while_revalidate(duration);
        self
    }

    pub fn with_refresh_before(mut self, duration: Duration) -> Self {
        self.policy = self.policy.clone().with_refresh_before(duration);
        self
    }

    pub fn with_max_body_size(mut self, size: Option<usize>) -> Self {
        self.policy = self.policy.clone().with_max_body_size(size);
        self
    }

    pub fn with_min_body_size(mut self, size: Option<usize>) -> Self {
        self.policy = self.policy.clone().with_min_body_size(size);
        self
    }

    pub fn with_allow_streaming_bodies(mut self, allow: bool) -> Self {
        self.policy = self.policy.clone().with_allow_streaming_bodies(allow);
        self
    }

    pub fn with_compression(mut self, config: CompressionConfig) -> Self {
        self.policy = self.policy.clone().with_compression(config);
        self
    }

    pub fn with_respect_cache_control(mut self, enabled: bool) -> Self {
        self.policy = self.policy.clone().with_respect_cache_control(enabled);
        self
    }

    pub fn with_cache_statuses(mut self, statuses: impl IntoIterator<Item = u16>) -> Self {
        self.policy = self.policy.clone().with_statuses(statuses);
        self
    }

    pub fn with_method_predicate<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&Method) -> bool + Send + Sync + 'static,
    {
        self.policy = self.policy.clone().with_method_predicate(predicate);
        self
    }

    pub fn with_header_allowlist<I, S>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.policy = self.policy.clone().with_header_allowlist(headers);
        self
    }

    pub fn with_key_extractor(mut self, extractor: KeyExtractor) -> Self {
        self.key_extractor = extractor;
        self
    }

    /// Configures how requests carrying a `Cookie` header interact with the cache.
    pub fn with_cookie_policy(mut self, cookie_policy: CookiePolicy) -> Self {
        self.policy = self.policy.clone().with_cookie_policy(cookie_policy);
        self
    }

    /// Returns the shared [`ChunkCache`] instance if chunk caching is enabled.
    pub fn chunk_cache(&self) -> Option<&Arc<ChunkCache>> {
        self.chunk_cache.as_ref()
    }

    /// Returns a reference to the underlying cache backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Returns the shared [`GlobalStats`] tracker wired into services produced by this layer.
    #[cfg(feature = "admin-api")]
    pub fn stats(&self) -> Arc<GlobalStats> {
        Arc::clone(&self.stats)
    }

    /// Invalidates a single key across both the configured backend and the in-memory chunk cache.
    pub async fn invalidate(&self, key: &str) -> Result<(), crate::error::CacheError> {
        if let Some(ref chunk_cache) = self.chunk_cache {
            chunk_cache.remove(key);
        }
        self.backend.invalidate(key).await?;
        #[cfg(feature = "admin-api")]
        self.stats.record_invalidation();
        #[cfg(feature = "serde")]
        {
            let ml_config = self.policy.ml_logging();
            if ml_config.enabled {
                CacheEvent::new(
                    CacheEventType::Invalidate,
                    RequestId::new(),
                    key.to_string(),
                )
                .log(ml_config);
            }
        }
        Ok(())
    }

    /// Invalidates all entries associated with `tag` across the backend and chunk cache.
    pub async fn invalidate_by_tag(&self, tag: &str) -> Result<usize, crate::error::CacheError> {
        if let Some(ref chunk_cache) = self.chunk_cache {
            if let Ok(keys) = self.backend.get_keys_by_tag(tag).await {
                for key in keys {
                    chunk_cache.remove(&key);
                }
            }
        }
        let count = self.backend.invalidate_by_tag(tag).await?;
        #[cfg(feature = "admin-api")]
        for _ in 0..count {
            self.stats.record_invalidation();
        }
        #[cfg(feature = "serde")]
        {
            let ml_config = self.policy.ml_logging();
            if ml_config.enabled {
                CacheEvent::new(
                    CacheEventType::TagInvalidate,
                    RequestId::new(),
                    tag.to_string(),
                )
                .log(ml_config);
            }
        }
        Ok(count)
    }

    /// Manually initialize the auto-refresh manager with a service instance.
    ///
    /// This should be called after constructing the service to start the background
    /// refresh task. This is only necessary if auto-refresh is enabled.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # use std::convert::Infallible;
    /// # use std::time::Duration;
    /// # use tower_http_cache::prelude::*;
    /// # use tower_http_cache::refresh::AutoRefreshConfig;
    /// # let backend = InMemoryBackend::new(100);
    /// # let my_service = tower::service_fn(|_req: http::Request<()>| async {
    /// #     Ok::<_, Infallible>(http::Response::new(http_body_util::Full::from("ok")))
    /// # });
    /// let layer = CacheLayer::builder(backend)
    ///     .refresh_before(Duration::from_secs(10))
    ///     .auto_refresh(AutoRefreshConfig::enabled(5.0))
    ///     .build();
    ///
    /// layer.init_auto_refresh(my_service.clone()).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn init_auto_refresh<S, ResBody>(&self, service: S) -> Result<(), String>
    where
        S: Service<Request<()>, Response = Response<ResBody>> + Clone + Send + Sync + 'static,
        S::Future: Send + 'static,
        S::Error: Into<BoxError> + Send,
        ResBody: Body<Data = Bytes> + Send + 'static,
        ResBody::Error: Into<BoxError> + Send,
        B: Clone,
    {
        if let Some(ref manager) = self.refresh_manager {
            let callback = Arc::new(CacheRefreshCallback::new(
                service,
                self.backend.clone(),
                self.policy.clone(),
                self.chunk_cache.clone(),
                Some(Arc::downgrade(manager)),
            ));
            manager.start(callback).await
        } else {
            Ok(())
        }
    }
}

impl<S, B> Layer<S> for CacheLayer<B>
where
    B: CacheBackend,
{
    type Service = CacheService<S, B>;

    fn layer(&self, inner: S) -> Self::Service {
        CacheService {
            inner,
            backend: self.backend.clone(),
            policy: self.policy.clone(),
            key_extractor: self.key_extractor.clone(),
            locks: self.locks.clone(),
            refresh_manager: self.refresh_manager.clone(),
            refresh_shutdown: self.refresh_shutdown.clone(),
            chunk_cache: self.chunk_cache.clone(),
            #[cfg(feature = "admin-api")]
            stats: self.stats.clone(),
        }
    }
}

#[derive(Clone)]
pub struct CacheService<S, B> {
    inner: S,
    backend: B,
    policy: CachePolicy,
    key_extractor: KeyExtractor,
    locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
    refresh_manager: Option<Arc<RefreshManager>>,
    #[allow(dead_code)]
    refresh_shutdown: Option<Arc<RefreshShutdownGuard>>,
    chunk_cache: Option<Arc<ChunkCache>>,
    #[cfg(feature = "admin-api")]
    stats: Arc<GlobalStats>,
}

/// Implementation of RefreshCallback for CacheService.
struct CacheRefreshCallback<S, B> {
    inner: S,
    backend: B,
    policy: CachePolicy,
    chunk_cache: Option<Arc<ChunkCache>>,
    refresh_manager: Option<Weak<RefreshManager>>,
}

impl<S, B> CacheRefreshCallback<S, B> {
    fn new(
        inner: S,
        backend: B,
        policy: CachePolicy,
        chunk_cache: Option<Arc<ChunkCache>>,
        refresh_manager: Option<Weak<RefreshManager>>,
    ) -> Self {
        Self {
            inner,
            backend,
            policy,
            chunk_cache,
            refresh_manager,
        }
    }
}

impl<S, B, ResBody> RefreshCallback for CacheRefreshCallback<S, B>
where
    S: Service<Request<()>, Response = Response<ResBody>> + Clone + Send + Sync + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send,
    ResBody: Body<Data = Bytes> + Send + 'static,
    ResBody::Error: Into<BoxError> + Send,
    B: CacheBackend,
{
    fn refresh(&self, key: String, metadata: RefreshMetadata) -> crate::refresh::RefreshFuture {
        let backend = self.backend.clone();
        let policy = self.policy.clone();
        let inner = self.inner.clone();
        let chunk_cache = self.chunk_cache.clone();
        let refresh_manager = self.refresh_manager.clone();

        Box::pin(async move {
            #[cfg(feature = "tracing")]
            tracing::debug!(key = %key, uri = %metadata.uri, "Auto-refresh triggered");

            // Captured before `try_into_request` consumes the metadata: the
            // tag extractor keys off the request, not the response.
            let request_method = metadata.method.clone();
            let request_uri = metadata.uri.clone();
            let is_head = request_method == Method::HEAD;
            let has_authorization = metadata
                .headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("authorization"));
            let has_cookie = metadata
                .headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("cookie"));
            let cookie_policy = policy.cookie_policy();

            if is_head || (has_cookie && matches!(cookie_policy, CookiePolicy::Bypass)) {
                return Ok(());
            }
            let auth_check =
                SharedAuthCheck::from_request(has_authorization, has_cookie, cookie_policy);

            // Reconstruct the request
            let request = match metadata.try_into_request() {
                Some(req) => req,
                None => {
                    #[cfg(feature = "tracing")]
                    tracing::warn!(key = %key, "Failed to reconstruct request for auto-refresh");
                    return Err("Failed to reconstruct request".into());
                }
            };

            // Call the inner service
            let service = inner;
            let response = match service.oneshot(request).await {
                Ok(resp) => resp,
                Err(_err) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!(key = %key, "Service error during auto-refresh");
                    return Err("Service error during refresh".into());
                }
            };

            let (parts, body) = response.into_parts();

            let content_type = parts
                .headers
                .get(http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok());
            let content_length = parts
                .headers
                .get(http::header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let size_hint = body.size_hint();
            let streaming_decision = should_stream(
                policy.streaming_policy(),
                &size_hint,
                content_type,
                content_length,
            );
            if matches!(streaming_decision, StreamingDecision::SkipCache) {
                return Ok(());
            }

            let streaming = size_hint.upper().is_none();

            // Collect the body
            let collected = match BodyExt::collect(body).await {
                Ok(c) => c,
                Err(_err) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!(key = %key, "Body collection error during auto-refresh");
                    return Err("Body collection error".into());
                }
            };

            let cache_bytes = collected.to_bytes();

            if !should_store_response(
                &parts.headers,
                &policy,
                &cache_bytes,
                streaming,
                is_head,
                auth_check,
            ) {
                return Ok(()); // Successfully refreshed but not stored
            }

            // Store the refreshed entry
            if let Some(ttl) = policy.ttl_for(parts.status) {
                if !ttl.is_zero() {
                    if let Some(ref cc) = chunk_cache {
                        populate_chunk_cache(cc, &key, &parts, &policy, &cache_bytes, ttl);
                    }

                    let stale_for = policy.stale_while_revalidate();
                    let (compressed_bytes, headers_to_cache, compressed) =
                        prepare_cached_payload_and_headers(&parts.headers, &policy, cache_bytes);

                    let entry = CacheEntry::new(
                        parts.status,
                        parts.version,
                        headers_to_cache,
                        compressed_bytes,
                    )
                    .with_compressed(compressed);
                    let tags = policy.extract_tags(&request_method, &request_uri);
                    let entry = if tags.is_empty() {
                        entry
                    } else {
                        entry.with_tags(tags)
                    };

                    if let Err(_err) = backend.set(key.clone(), entry, ttl, stale_for).await {
                        #[cfg(feature = "tracing")]
                        tracing::error!(key = %key, "Failed to store refreshed entry");
                        return Err("Failed to store entry".into());
                    }

                    if let Some(manager) = refresh_manager.as_ref().and_then(Weak::upgrade) {
                        manager.update_expiry(&key, SystemTime::now() + ttl);
                    }
                }
            }

            Ok(())
        })
    }
}

// Note: We cannot easily initialize the refresh manager from within the service
// because the service may have different type parameters than required by the callback.
// Instead, users who want to use auto-refresh should ensure the service is called at least once,
// or manually initialize the refresh functionality if needed.

impl<S, B, ReqBody, ResBody> Service<Request<ReqBody>> for CacheService<S, B>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send,
    ReqBody: Send + 'static,
    ResBody: Body<Data = Bytes> + Send + 'static,
    ResBody::Error: Into<BoxError> + Send,
    B: CacheBackend,
{
    type Response = Response<SyncBoxBody>;
    type Error = BoxError;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let method = req.method().clone();
        let uri = req.uri().clone();
        let is_head = method == Method::HEAD;
        let should_cache_method = self.policy.should_cache_method(&method);
        let request_bypass =
            self.policy.respect_cache_control() && cache_control_disallows(req.headers());
        let has_cookie = req.headers().contains_key(http::header::COOKIE);
        let has_authorization = req.headers().contains_key(http::header::AUTHORIZATION);
        let cookie_policy = self.policy.cookie_policy();
        let cookie_bypass = has_cookie && matches!(cookie_policy, CookiePolicy::Bypass);
        let auth_check =
            SharedAuthCheck::from_request(has_authorization, has_cookie, cookie_policy);
        let accept_encoding = AcceptEncoding::from_headers(req.headers());
        let accept_gzip = accept_encoding.accepts("gzip");
        let has_range_header = req.headers().contains_key(http::header::RANGE);
        let req_if_range = req
            .headers()
            .get(http::header::IF_RANGE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim().to_owned());

        let key = if should_cache_method && !request_bypass && !cookie_bypass {
            self.key_extractor.extract(&method, &uri)
        } else {
            None
        };

        let backend = self.backend.clone();
        let policy = self.policy.clone();
        let locks = self.locks.clone();
        let inner = self.inner.clone();
        let stale_window = policy.stale_while_revalidate();
        let refresh_before = policy.refresh_before();
        let refresh_manager = self.refresh_manager.clone();
        let chunk_cache = self.chunk_cache.clone();
        #[cfg(feature = "admin-api")]
        let stats = self.stats.clone();
        #[cfg(feature = "serde")]
        let ml_ctx = MlLogContext::from_request(policy.ml_logging(), &req);
        #[cfg(feature = "serde")]
        let call_start = ml_ctx.as_ref().map(|_| std::time::Instant::now());

        // Check for range request early
        let range_request = parse_range_header(req.headers());

        // Prepare refresh metadata if auto-refresh is enabled
        let refresh_metadata = if refresh_manager.is_some() && key.is_some() && !is_head {
            Some(RefreshMetadata::from_request(&req))
        } else {
            None
        };

        Box::pin(async move {
            #[cfg(feature = "tracing")]
            tracing::debug!(method = %method, uri = %uri, "cache_call");

            #[cfg(feature = "serde")]
            if key.is_none() {
                if let Some(ref ml) = ml_ctx {
                    ml.event(CacheEventType::Bypass, uri.to_string())
                        .log(&ml.config);
                }
            }

            // Try to serve from chunk cache if this is a range request
            if let (Some(range_req), Some(chunk_cache), Some(key_ref)) =
                (range_request.as_ref(), &chunk_cache, &key)
            {
                if let Some(entry) = chunk_cache.get(key_ref) {
                    let auth_allowed =
                        cached_entry_allows_auth_and_vary(&entry.metadata.headers, auth_check);
                    let encoding_allowed =
                        upstream_encoding_accepted(&entry.metadata.headers, &accept_encoding);
                    let if_range_ok = if_range_matches(
                        req_if_range.as_deref(),
                        entry.metadata.etag.as_deref(),
                        entry.metadata.last_modified.as_deref(),
                    );
                    if auth_allowed && encoding_allowed && if_range_ok {
                        // Check if range is satisfiable and normalize it
                        if let Some(normalized) = range_req.normalize(entry.metadata.total_size) {
                            let end = normalized
                                .end
                                .unwrap_or_else(|| entry.metadata.total_size.saturating_sub(1));

                            // Try to get range from chunks
                            if let Some(range_data) = entry.get_range(normalized.start, end) {
                                let range_len = range_data.len();
                                #[cfg(feature = "metrics")]
                                counter!("tower_http_cache.chunk_cache_hit").increment(1);
                                #[cfg(feature = "admin-api")]
                                stats.record_hit(key_ref);

                                #[cfg(feature = "tracing")]
                                tracing::debug!(
                                    key = %key_ref,
                                    start = normalized.start,
                                    end = end,
                                    "chunk_cache_hit"
                                );

                                let response = build_chunk_hit_response(
                                    &entry,
                                    if is_head { Bytes::new() } else { range_data },
                                    normalized.start,
                                    end,
                                )?;

                                #[cfg(feature = "serde")]
                                if let (Some(ml), Some(t0)) = (&ml_ctx, call_start) {
                                    ml.event(CacheEventType::Hit, key_ref.as_str())
                                        .with_hit(true)
                                        .with_status(response.status())
                                        .with_version(response.version())
                                        .with_size(range_len)
                                        .with_latency(t0.elapsed())
                                        .log(&ml.config);
                                }

                                let _ = range_len;
                                return Ok(response);
                            }
                        }
                    }
                }

                #[cfg(feature = "metrics")]
                counter!("tower_http_cache.chunk_cache_miss").increment(1);
            }

            let mut stale_entry: Option<CacheEntry> = None;
            if let Some(ref key_ref) = key {
                if let Ok(Some(hit)) = backend.get(key_ref).await {
                    match classify_hit(hit, stale_window, refresh_before) {
                        HitState::Fresh(entry)
                            if is_usable_hit(&entry, auth_check, &accept_encoding) =>
                        {
                            #[cfg(feature = "metrics")]
                            counter!("tower_http_cache.hit").increment(1);
                            #[cfg(feature = "admin-api")]
                            stats.record_hit(key_ref);

                            // Record hit for auto-refresh tracking
                            if let Some(ref manager) = refresh_manager {
                                manager.tracker().record_hit(key_ref);
                            }

                            #[cfg(feature = "serde")]
                            if let (Some(ml), Some(t0)) = (&ml_ctx, call_start) {
                                ml.event(CacheEventType::Hit, key_ref.as_str())
                                    .with_hit(true)
                                    .with_status(entry.status)
                                    .with_version(entry.version)
                                    .with_size(entry.body.len())
                                    .with_latency(t0.elapsed())
                                    .log(&ml.config);
                            }

                            return Ok(entry.into_response_with_options(accept_gzip, is_head));
                        }
                        HitState::Stale(entry)
                            if is_usable_hit(&entry, auth_check, &accept_encoding) =>
                        {
                            #[cfg(feature = "metrics")]
                            counter!("tower_http_cache.stale_hit").increment(1);

                            // Record hit for auto-refresh tracking
                            if let Some(ref manager) = refresh_manager {
                                manager.tracker().record_hit(key_ref);
                            }

                            stale_entry = Some(entry);
                        }
                        _ => {}
                    }
                }
            }

            let mut primary_guard: Option<StampedeGuard> = None;
            if let Some(ref key_ref) = key {
                match StampedeGuard::acquire_handle(locks.clone(), key_ref.clone()).await {
                    StampedeHandle::Primary(guard) => {
                        primary_guard = Some(guard);
                    }
                    StampedeHandle::Secondary(lock) => {
                        if let Some(entry) = stale_entry.clone() {
                            #[cfg(feature = "metrics")]
                            counter!("tower_http_cache.stale_served").increment(1);
                            #[cfg(feature = "admin-api")]
                            stats.record_stale_hit(key_ref);
                            #[cfg(feature = "serde")]
                            if let (Some(ml), Some(t0)) = (&ml_ctx, call_start) {
                                ml.event(CacheEventType::StaleHit, key_ref.as_str())
                                    .with_hit(true)
                                    .with_status(entry.status)
                                    .with_version(entry.version)
                                    .with_size(entry.body.len())
                                    .with_latency(t0.elapsed())
                                    .log(&ml.config);
                            }
                            return Ok(entry.into_response_with_options(accept_gzip, is_head));
                        }

                        let secondary_guard = lock.lock_owned().await;
                        drop(secondary_guard);

                        if let Ok(Some(hit)) = backend.get(key_ref).await {
                            match classify_hit(hit, stale_window, refresh_before) {
                                HitState::Fresh(entry)
                                    if is_usable_hit(&entry, auth_check, &accept_encoding) =>
                                {
                                    #[cfg(feature = "metrics")]
                                    counter!("tower_http_cache.hit_after_wait").increment(1);
                                    #[cfg(feature = "admin-api")]
                                    stats.record_hit(key_ref);
                                    #[cfg(feature = "serde")]
                                    if let (Some(ml), Some(t0)) = (&ml_ctx, call_start) {
                                        ml.event(CacheEventType::Hit, key_ref.as_str())
                                            .with_hit(true)
                                            .with_status(entry.status)
                                            .with_version(entry.version)
                                            .with_size(entry.body.len())
                                            .with_latency(t0.elapsed())
                                            .log(&ml.config);
                                    }
                                    return Ok(
                                        entry.into_response_with_options(accept_gzip, is_head)
                                    );
                                }
                                HitState::Stale(entry)
                                    if is_usable_hit(&entry, auth_check, &accept_encoding) =>
                                {
                                    #[cfg(feature = "metrics")]
                                    counter!("tower_http_cache.stale_served").increment(1);
                                    #[cfg(feature = "admin-api")]
                                    stats.record_stale_hit(key_ref);
                                    #[cfg(feature = "serde")]
                                    if let (Some(ml), Some(t0)) = (&ml_ctx, call_start) {
                                        ml.event(CacheEventType::StaleHit, key_ref.as_str())
                                            .with_hit(true)
                                            .with_status(entry.status)
                                            .with_version(entry.version)
                                            .with_size(entry.body.len())
                                            .with_latency(t0.elapsed())
                                            .log(&ml.config);
                                    }
                                    return Ok(
                                        entry.into_response_with_options(accept_gzip, is_head)
                                    );
                                }
                                _ => {}
                            }
                        }

                        if let StampedeHandle::Primary(guard) =
                            StampedeGuard::acquire_handle(locks.clone(), key_ref.clone()).await
                        {
                            primary_guard = Some(guard);
                        }
                    }
                }

                // Double-checked read. The lookup above and this acquisition
                // are not one atomic step: another caller can take the lock,
                // fetch, store and release inside that window, so winning the
                // lock does not mean the key still needs fetching. Without
                // this, two requests in flight at the same time each hit the
                // origin -- the stampede the lock exists to prevent. The
                // waiting branch above re-reads for the same reason. Only a
                // fresh entry short-circuits; stale or expired still owes the
                // origin a request.
                if primary_guard.is_some() {
                    if let Ok(Some(hit)) = backend.get(key_ref).await {
                        if let HitState::Fresh(entry) =
                            classify_hit(hit, stale_window, refresh_before)
                        {
                            if is_usable_hit(&entry, auth_check, &accept_encoding) {
                                #[cfg(feature = "metrics")]
                                counter!("tower_http_cache.hit_after_lock").increment(1);
                                #[cfg(feature = "admin-api")]
                                stats.record_hit(key_ref);
                                #[cfg(feature = "serde")]
                                if let (Some(ml), Some(t0)) = (&ml_ctx, call_start) {
                                    ml.event(CacheEventType::Hit, key_ref.as_str())
                                        .with_hit(true)
                                        .with_status(entry.status)
                                        .with_version(entry.version)
                                        .with_size(entry.body.len())
                                        .with_latency(t0.elapsed())
                                        .log(&ml.config);
                                }
                                return Ok(entry.into_response_with_options(accept_gzip, is_head));
                            }
                        }
                    }
                }
            }

            #[cfg(feature = "metrics")]
            counter!("tower_http_cache.miss").increment(1);
            #[cfg(feature = "admin-api")]
            if let Some(ref key_ref) = key {
                stats.record_miss(key_ref);
            }

            #[cfg(feature = "metrics")]
            let start = std::time::Instant::now();
            let service = inner;
            let response = service.oneshot(req).await.map_err(|err| err.into())?;
            #[cfg(feature = "metrics")]
            histogram!("tower_http_cache.backend_latency").record(start.elapsed().as_secs_f64());

            let (mut parts, body) = response.into_parts();

            #[cfg(feature = "serde")]
            if let (Some(ml), Some(t0), Some(miss_key)) = (&ml_ctx, call_start, key.as_deref()) {
                ml.event(CacheEventType::Miss, miss_key)
                    .with_hit(false)
                    .with_status(parts.status)
                    .with_version(parts.version)
                    .with_latency(t0.elapsed())
                    .log(&ml.config);
            }

            // NEW: Early streaming decision
            let content_type = parts
                .headers
                .get(http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok());

            let content_length = parts
                .headers
                .get(http::header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());

            let size_hint = body.size_hint();

            let streaming_decision = should_stream(
                policy.streaming_policy(),
                &size_hint,
                content_type,
                content_length,
            );

            // Check for range requests
            let is_range_request = has_range_header;
            let is_partial_response = is_partial_content(parts.status);
            let range_handling = policy.streaming_policy().range_handling;

            // Handle range requests according to policy
            if (is_range_request || is_partial_response)
                && range_handling == RangeHandling::PassThrough
            {
                #[cfg(feature = "metrics")]
                counter!("tower_http_cache.range_request_passthrough").increment(1);

                #[cfg(feature = "tracing")]
                tracing::debug!(
                    method = %method,
                    uri = %uri,
                    is_range = is_range_request,
                    is_partial = is_partial_response,
                    "range_request_passthrough"
                );

                // Stream through without buffering for range requests
                let boxed_body = SyncBoxBody::from_body(body);
                drop(primary_guard);
                return Ok(Response::from_parts(parts, boxed_body));
            }

            // Check if we should skip caching and pass through
            if matches!(streaming_decision, StreamingDecision::SkipCache) {
                // TRUE STREAMING: Pass through without buffering!
                #[cfg(feature = "metrics")]
                counter!("tower_http_cache.streaming_passthrough").increment(1);

                #[cfg(feature = "tracing")]
                tracing::debug!(
                    method = %method,
                    uri = %uri,
                    decision = ?streaming_decision,
                    content_type = ?content_type,
                    size = ?extract_size_info(&size_hint, content_length),
                    "streaming_passthrough"
                );

                // Box the body and stream it through without collecting
                let boxed_body = SyncBoxBody::from_body(body);
                drop(primary_guard);
                return Ok(Response::from_parts(parts, boxed_body));
            }

            let streaming = body.size_hint().upper().is_none();
            if streaming && !policy.allow_streaming_bodies() {
                #[cfg(feature = "metrics")]
                counter!("tower_http_cache.streaming_skip").increment(1);
            }

            let collected = BodyExt::collect(body).await.map_err(|err| err.into())?;
            let cache_bytes = collected.to_bytes();
            let mut response_bytes = cache_bytes.clone();

            let should_store = key.is_some()
                && should_store_response(
                    &parts.headers,
                    &policy,
                    &cache_bytes,
                    streaming,
                    is_head,
                    auth_check,
                );

            let version = parts.version;
            let status = parts.status;

            if should_store {
                if let Some(key_ref) = &key {
                    if let Some(ttl) = policy.ttl_for(status) {
                        if !ttl.is_zero() {
                            if let Some(ref chunk_cache) = chunk_cache {
                                populate_chunk_cache(
                                    chunk_cache,
                                    key_ref,
                                    &parts,
                                    &policy,
                                    &cache_bytes,
                                    ttl,
                                );
                            }

                            let stale_for = policy.stale_while_revalidate();
                            let (compressed_bytes, headers_to_cache, compressed) =
                                prepare_cached_payload_and_headers(
                                    &parts.headers,
                                    &policy,
                                    cache_bytes,
                                );
                            let stored_size = compressed_bytes.len();
                            if compressed {
                                #[cfg(feature = "metrics")]
                                counter!("tower_http_cache.compressed").increment(1);

                                merge_vary_accept_encoding_header_map(&mut parts.headers);
                                if accept_gzip {
                                    parts.headers.insert(
                                        http::header::CONTENT_ENCODING,
                                        http::HeaderValue::from_static("gzip"),
                                    );
                                    if let Ok(len_val) = http::HeaderValue::from_str(
                                        &compressed_bytes.len().to_string(),
                                    ) {
                                        parts.headers.insert(http::header::CONTENT_LENGTH, len_val);
                                    }
                                    tag_gzip_etag_header_map(&mut parts.headers);
                                    response_bytes = compressed_bytes.clone();
                                }
                            }
                            let entry = CacheEntry::new(
                                status,
                                version,
                                headers_to_cache,
                                compressed_bytes,
                            )
                            .with_compressed(compressed);
                            // `extract_tags` returns an empty vector unless
                            // `TagPolicy::enabled` is set, which defaults to
                            // false -- so this is inert for anyone who has not
                            // opted in.
                            let tags = policy.extract_tags(&method, &uri);
                            let entry = if tags.is_empty() {
                                entry
                            } else {
                                entry.with_tags(tags.clone())
                            };
                            if backend
                                .set(key_ref.clone(), entry, ttl, stale_for)
                                .await
                                .is_err()
                            {
                                #[cfg(feature = "metrics")]
                                counter!("tower_http_cache.store_error").increment(1);
                            } else {
                                #[cfg(feature = "metrics")]
                                counter!("tower_http_cache.store").increment(1);
                                #[cfg(feature = "admin-api")]
                                stats.record_store(key_ref);

                                #[cfg(feature = "serde")]
                                if let Some(ref ml) = ml_ctx {
                                    let mut event = ml
                                        .event(CacheEventType::Store, key_ref.as_str())
                                        .with_status(status)
                                        .with_version(version)
                                        .with_size(stored_size)
                                        .with_ttl(ttl);
                                    if !tags.is_empty() {
                                        event = event.with_tags(tags);
                                    }
                                    event.log(&ml.config);
                                }
                                let _ = stored_size;

                                // Store refresh metadata if auto-refresh is enabled
                                if let (Some(manager), Some(metadata)) =
                                    (&refresh_manager, refresh_metadata)
                                {
                                    manager.store_metadata_with_expiry(
                                        key_ref.clone(),
                                        metadata,
                                        SystemTime::now() + ttl,
                                    );
                                }
                            }
                        }
                    }
                }
            } else {
                #[cfg(feature = "metrics")]
                counter!("tower_http_cache.store_skipped").increment(1);
            }

            drop(primary_guard);

            // Box the response body; preserve GET body length on HEAD when upstream omitted it.
            if is_head && !parts.headers.contains_key(http::header::CONTENT_LENGTH) {
                if let Ok(len_val) = http::HeaderValue::from_str(&response_bytes.len().to_string())
                {
                    parts.headers.insert(http::header::CONTENT_LENGTH, len_val);
                }
            }
            let full_body = Full::from(if is_head {
                Bytes::new()
            } else {
                response_bytes
            });
            let boxed_body = SyncBoxBody::new(full_body.map_err(Into::into).boxed());
            Ok(Response::from_parts(parts, boxed_body))
        })
    }
}

struct StampedeGuard {
    key: String,
    locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
    lock: Arc<Mutex<()>>,
    _guard: OwnedMutexGuard<()>,
}

enum StampedeHandle {
    Primary(StampedeGuard),
    Secondary(Arc<Mutex<()>>),
}

impl StampedeGuard {
    async fn acquire_handle(
        locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
        key: String,
    ) -> StampedeHandle {
        // The `let` binding is deliberate: `DashMap::entry` returns a guard
        // holding the shard's write lock, and binding the match result pins
        // the drop point to the end of this statement -- identically in both
        // edition 2021 and 2024. Edition 2024 changed when tail-expression
        // temporaries drop, which is why clippy only started flagging this as
        // redundant after the edition bump. Returning the `match` directly
        // would make lock release depend on those edition rules; in a path
        // that acquires a stampede lock and then awaits, that is not a
        // trade worth making to save one line.
        #[allow(clippy::let_and_return)]
        let handle = match locks.entry(key.clone()) {
            Entry::Occupied(entry) => StampedeHandle::Secondary(entry.get().clone()),
            Entry::Vacant(entry) => {
                let lock = Arc::new(Mutex::new(()));
                entry.insert(lock.clone());
                let guard = lock.clone().lock_owned().await;
                let locks_clone = locks.clone();
                StampedeHandle::Primary(StampedeGuard {
                    key,
                    locks: locks_clone,
                    lock,
                    _guard: guard,
                })
            }
        };
        handle
    }
}

impl Drop for StampedeGuard {
    fn drop(&mut self) {
        if let Some(current) = self.locks.get(&self.key) {
            let should_remove = Arc::ptr_eq(&self.lock, current.value());
            drop(current);
            if should_remove {
                self.locks.remove(&self.key);
            }
        }
    }
}

fn classify_hit(hit: CacheRead, stale_window: Duration, refresh_before: Duration) -> HitState {
    let now = SystemTime::now();
    let CacheRead {
        entry,
        expires_at,
        stale_until,
    } = hit;

    if let Some(expires_at) = expires_at {
        if expires_at > now {
            if refresh_before > Duration::ZERO {
                if let Some(threshold) = expires_at.checked_sub(refresh_before) {
                    if now >= threshold {
                        return HitState::Stale(entry);
                    }
                } else {
                    return HitState::Stale(entry);
                }
            }
            return HitState::Fresh(entry);
        }
    }

    if stale_window > Duration::ZERO {
        if let Some(stale_until) = stale_until {
            if stale_until > now {
                return HitState::Stale(entry);
            }
        }
    }

    HitState::Expired
}

#[derive(Debug)]
enum HitState {
    Fresh(CacheEntry),
    Stale(CacheEntry),
    Expired,
}

fn cache_control_disallows(headers: &HeaderMap) -> bool {
    headers
        .get_all(CACHE_CONTROL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|token| token.trim().to_ascii_lowercase())
        .any(|token| matches!(token.as_str(), "no-store" | "no-cache" | "private"))
        || headers
            .get(PRAGMA)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_ascii_lowercase().contains("no-cache"))
            .unwrap_or(false)
}

#[cfg(feature = "serde")]
struct MlLogContext {
    config: MLLoggingConfig,
    request_id: RequestId,
    method: Method,
    uri: Uri,
}

#[cfg(feature = "serde")]
impl MlLogContext {
    fn from_request<ReqBody>(config: &MLLoggingConfig, req: &Request<ReqBody>) -> Option<Self> {
        if !config.enabled {
            return None;
        }
        Some(Self {
            config: config.clone(),
            request_id: RequestId::from_request(req),
            method: req.method().clone(),
            uri: req.uri().clone(),
        })
    }

    fn event(&self, event_type: CacheEventType, key: impl Into<String>) -> CacheEvent {
        CacheEvent::new(event_type, self.request_id.clone(), key.into())
            .with_method(self.method.clone())
            .with_uri(self.uri.clone())
    }
}

fn build_chunk_hit_response(
    entry: &ChunkedEntry,
    range_data: Bytes,
    start: u64,
    end: u64,
) -> Result<Response<SyncBoxBody>, BoxError> {
    let mut response = Response::new(SyncBoxBody::new(
        Full::from(range_data).map_err(Into::into).boxed(),
    ));
    *response.status_mut() = http::StatusCode::PARTIAL_CONTENT;
    *response.version_mut() = entry.metadata.version;

    for (name, value) in &entry.metadata.headers {
        if let (Ok(header_name), Ok(header_value)) = (
            http::header::HeaderName::from_bytes(name.as_bytes()),
            http::header::HeaderValue::from_bytes(value),
        ) {
            response.headers_mut().append(header_name, header_value);
        }
    }

    let content_range = format!("bytes {}-{}/{}", start, end, entry.metadata.total_size);
    let content_range_val =
        http::header::HeaderValue::from_str(&content_range).map_err(BoxError::from)?;
    response
        .headers_mut()
        .insert(http::header::CONTENT_RANGE, content_range_val);

    let content_length = (end - start + 1).to_string();
    let content_length_val =
        http::header::HeaderValue::from_str(&content_length).map_err(BoxError::from)?;
    response
        .headers_mut()
        .insert(http::header::CONTENT_LENGTH, content_length_val);

    Ok(response)
}

fn populate_chunk_cache(
    chunk_cache: &ChunkCache,
    key: &str,
    parts: &http::response::Parts,
    policy: &CachePolicy,
    cache_bytes: &Bytes,
    ttl: Duration,
) {
    let streaming_policy = policy.streaming_policy();
    if !streaming_policy.enable_chunk_cache
        || (cache_bytes.len() as u64) < streaming_policy.min_chunk_file_size
        || !parts.status.is_success()
    {
        return;
    }

    #[cfg(feature = "tracing")]
    tracing::debug!(
        key = %key,
        size = cache_bytes.len(),
        chunk_size = streaming_policy.chunk_size,
        "populating_chunk_cache"
    );

    let mut cached_headers = policy.headers_to_cache(&parts.headers);
    preserve_upstream_encoding_headers(&mut cached_headers, &parts.headers);
    preserve_auth_vary_header(&mut cached_headers, &parts.headers);

    let metadata = ChunkMetadata {
        total_size: cache_bytes.len() as u64,
        content_type: parts
            .headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string(),
        etag: parts
            .headers
            .get(http::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
        last_modified: parts
            .headers
            .get(http::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()),
        status: parts.status,
        version: parts.version,
        headers: cached_headers,
    };

    let chunk_size = streaming_policy.chunk_size.max(1);
    let mut chunks = Vec::new();
    let mut offset = 0;

    while offset < cache_bytes.len() {
        let end = std::cmp::min(offset + chunk_size, cache_bytes.len());
        chunks.push(cache_bytes.slice(offset..end));
        offset = end;
    }

    let chunk_count = chunks.len();
    chunk_cache.replace_with_ttl(key.to_string(), metadata, Some(ttl), chunks);

    #[cfg(feature = "metrics")]
    counter!("tower_http_cache.chunk_cache_stored").increment(1);

    #[cfg(feature = "tracing")]
    tracing::debug!(key = %key, chunks = chunk_count, "chunk_cache_populated");
    let _ = chunk_count;
}

#[derive(Clone, Copy)]
struct SharedAuthCheck {
    requires_shared_public: bool,
    has_non_bypass_cookie: bool,
    has_authorization: bool,
}

impl SharedAuthCheck {
    fn from_request(
        has_authorization: bool,
        has_cookie: bool,
        cookie_policy: CookiePolicy,
    ) -> Self {
        Self {
            requires_shared_public: has_authorization
                || (has_cookie && matches!(cookie_policy, CookiePolicy::RequirePublic)),
            has_non_bypass_cookie: has_cookie && !matches!(cookie_policy, CookiePolicy::Bypass),
            has_authorization,
        }
    }

    fn checks_vary(self) -> bool {
        self.has_non_bypass_cookie || self.has_authorization
    }
}

fn should_store_response(
    headers: &HeaderMap,
    policy: &CachePolicy,
    cache_bytes: &[u8],
    streaming: bool,
    is_head: bool,
    auth_check: SharedAuthCheck,
) -> bool {
    let cache_control_block = policy.respect_cache_control() && cache_control_disallows(headers);
    let body_too_large = policy
        .max_body_size()
        .is_some_and(|max| cache_bytes.len() > max);
    let body_too_small = policy
        .min_body_size()
        .is_some_and(|min| cache_bytes.len() < min);
    let auth_allowed = !auth_check.requires_shared_public || response_allows_shared_auth(headers);
    let vary_allowed = response_vary_allows_request(headers, auth_check);

    !is_head
        && !cache_control_block
        && !body_too_large
        && !body_too_small
        && auth_allowed
        && vary_allowed
        && (policy.allow_streaming_bodies() || !streaming)
}

fn is_usable_hit(
    entry: &CacheEntry,
    auth_check: SharedAuthCheck,
    accept_encoding: &AcceptEncoding,
) -> bool {
    let auth_and_vary_ok = cached_entry_allows_auth_and_vary(&entry.headers, auth_check);
    let encoding_ok =
        entry.compressed || upstream_encoding_accepted(&entry.headers, accept_encoding);
    auth_and_vary_ok && encoding_ok
}

fn vary_str_blocks_request(value: &str, auth_check: SharedAuthCheck) -> bool {
    value.split(',').map(str::trim).any(|tok| {
        (tok == "*" && auth_check.checks_vary())
            || (auth_check.has_non_bypass_cookie && tok.eq_ignore_ascii_case("cookie"))
            || (auth_check.has_authorization && tok.eq_ignore_ascii_case("authorization"))
    })
}

fn response_vary_allows_request(headers: &HeaderMap, auth_check: SharedAuthCheck) -> bool {
    if !auth_check.checks_vary() {
        return true;
    }
    !headers.get_all(http::header::VARY).iter().any(|v| {
        v.to_str()
            .map_or(true, |s| vary_str_blocks_request(s, auth_check))
    })
}

fn cached_entry_vary_allows_request(
    headers: &[(String, Vec<u8>)],
    auth_check: SharedAuthCheck,
) -> bool {
    if !auth_check.checks_vary() {
        return true;
    }
    !headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("vary"))
        .any(|(_, value)| {
            std::str::from_utf8(value).map_or(true, |s| vary_str_blocks_request(s, auth_check))
        })
}

fn cached_entry_allows_auth_and_vary(
    headers: &[(String, Vec<u8>)],
    auth_check: SharedAuthCheck,
) -> bool {
    let auth_ok = !auth_check.requires_shared_public || cached_entry_allows_shared_auth(headers);
    auth_ok && cached_entry_vary_allows_request(headers, auth_check)
}

fn vary_str_mentions_shared_auth(value: &str) -> bool {
    value.split(',').map(str::trim).any(|tok| {
        tok == "*"
            || tok.eq_ignore_ascii_case("cookie")
            || tok.eq_ignore_ascii_case("authorization")
    })
}

fn preserve_auth_vary_header(cached_headers: &mut Vec<(String, Vec<u8>)>, source: &HeaderMap) {
    let mentions_auth_vary = source
        .get_all(http::header::VARY)
        .iter()
        .any(|v| v.to_str().map_or(true, vary_str_mentions_shared_auth));
    if mentions_auth_vary {
        preserve_header_if_missing(cached_headers, source, http::header::VARY);
    }
}

fn cache_control_allows_shared_auth_str(value: &str) -> bool {
    value.split(',').any(|directive| {
        let trimmed = directive.trim().to_ascii_lowercase();
        trimmed == "public" || trimmed == "must-revalidate" || trimmed.starts_with("s-maxage=")
    })
}

fn response_allows_shared_auth(headers: &HeaderMap) -> bool {
    headers
        .get_all(CACHE_CONTROL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(cache_control_allows_shared_auth_str)
}

fn cached_entry_allows_shared_auth(headers: &[(String, Vec<u8>)]) -> bool {
    headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("cache-control"))
        .filter_map(|(_, value)| std::str::from_utf8(value).ok())
        .any(cache_control_allows_shared_auth_str)
}

#[derive(Clone)]
struct AcceptEncoding {
    codings: Vec<(String, f32)>,
}

impl AcceptEncoding {
    fn from_headers(headers: &HeaderMap) -> Self {
        let mut codings = Vec::new();
        for value in headers.get_all(http::header::ACCEPT_ENCODING) {
            let Ok(raw) = value.to_str() else {
                continue;
            };
            for item in raw.split(',') {
                let mut parts = item.split(';');
                let coding = parts.next().map(str::trim).unwrap_or("");
                if coding.is_empty() {
                    continue;
                }
                let mut q = 1.0_f32;
                for param in parts {
                    let param = param.trim();
                    if let Some(q_str) = param
                        .strip_prefix("q=")
                        .or_else(|| param.strip_prefix("Q="))
                    {
                        if let Ok(parsed) = q_str.trim().parse::<f32>() {
                            q = parsed;
                        }
                    }
                }
                codings.push((coding.to_ascii_lowercase(), q));
            }
        }
        Self { codings }
    }

    fn accepts(&self, target: &str) -> bool {
        let mut target_q: Option<f32> = None;
        let mut star_q: Option<f32> = None;

        for (coding, q) in &self.codings {
            if coding.eq_ignore_ascii_case(target) {
                target_q = Some(*q);
            } else if coding == "*" {
                star_q = Some(*q);
            }
        }

        match target_q {
            Some(q) => q > 0.0,
            None => star_q.is_some_and(|q| q > 0.0),
        }
    }
}

fn upstream_encoding_accepted(
    headers: &[(String, Vec<u8>)],
    accept_encoding: &AcceptEncoding,
) -> bool {
    for (name, value) in headers {
        if !name.eq_ignore_ascii_case("content-encoding") {
            continue;
        }
        let Ok(raw) = std::str::from_utf8(value) else {
            return false;
        };
        for coding in raw.split(',').map(str::trim).filter(|c| !c.is_empty()) {
            if coding.eq_ignore_ascii_case("identity") {
                continue;
            }
            if !accept_encoding.accepts(coding) {
                return false;
            }
        }
    }
    true
}

fn if_range_matches(
    if_range: Option<&str>,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> bool {
    let Some(raw) = if_range else {
        return true;
    };
    let val = raw.trim();
    if val.is_empty() || val.starts_with("W/") {
        return false;
    }
    if val.starts_with('"') {
        etag.is_some_and(|e| {
            let e = e.trim();
            !e.starts_with("W/") && e == val
        })
    } else {
        last_modified.is_some_and(|lm| lm.trim() == val)
    }
}

fn preserve_header_if_missing(
    cached_headers: &mut Vec<(String, Vec<u8>)>,
    source: &HeaderMap,
    header_name: http::header::HeaderName,
) {
    if cached_headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(header_name.as_str()))
    {
        return;
    }
    for value in source.get_all(&header_name) {
        cached_headers.push((header_name.as_str().to_string(), value.as_bytes().to_vec()));
    }
}

fn vary_includes_accept_encoding(value: &str) -> bool {
    value
        .split(',')
        .map(str::trim)
        .any(|tok| tok == "*" || tok.eq_ignore_ascii_case("accept-encoding"))
}

fn merge_vary_accept_encoding_vec(headers: &mut Vec<(String, Vec<u8>)>) {
    for (name, value) in headers.iter_mut() {
        if name.eq_ignore_ascii_case("vary") {
            if let Ok(existing) = std::str::from_utf8(value) {
                if vary_includes_accept_encoding(existing) {
                    return;
                }
                let updated = if existing.trim().is_empty() {
                    "Accept-Encoding".to_string()
                } else {
                    format!("{}, Accept-Encoding", existing.trim())
                };
                *value = updated.into_bytes();
            }
            return;
        }
    }
    headers.push(("vary".to_string(), b"Accept-Encoding".to_vec()));
}

fn merge_vary_accept_encoding_header_map(headers: &mut HeaderMap) {
    let already_covered = headers
        .get_all(http::header::VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(vary_includes_accept_encoding);
    if !already_covered {
        headers.append(
            http::header::VARY,
            http::HeaderValue::from_static("Accept-Encoding"),
        );
    }
}

fn tag_gzip_etag_str(etag: &str) -> Option<String> {
    let trimmed = etag.trim();
    if trimmed.ends_with('"') && !trimmed.ends_with("-gzip\"") && trimmed.len() >= 2 {
        let prefix = &trimmed[..trimmed.len() - 1];
        Some(format!("{prefix}-gzip\""))
    } else {
        None
    }
}

fn tag_gzip_etag_header_map(headers: &mut HeaderMap) {
    if let Some(etag) = headers
        .get(http::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .and_then(tag_gzip_etag_str)
    {
        if let Ok(val) = http::HeaderValue::from_str(&etag) {
            headers.insert(http::header::ETAG, val);
        }
    }
}

fn preserve_upstream_encoding_headers(
    cached_headers: &mut Vec<(String, Vec<u8>)>,
    source: &HeaderMap,
) {
    if source.contains_key(http::header::CONTENT_ENCODING) {
        preserve_header_if_missing(cached_headers, source, http::header::CONTENT_ENCODING);
        preserve_header_if_missing(cached_headers, source, http::header::VARY);
    }
}

fn prepare_cached_payload_and_headers(
    headers: &HeaderMap,
    policy: &CachePolicy,
    raw_bytes: Bytes,
) -> (Bytes, Vec<(String, Vec<u8>)>, bool) {
    let mut cached_headers = policy.headers_to_cache(headers);
    preserve_auth_vary_header(&mut cached_headers, headers);

    // Never double-compress upstream payloads that already carry Content-Encoding,
    // and preserve Content-Encoding / Vary even when header_allowlist omits them.
    if headers.contains_key(http::header::CONTENT_ENCODING) {
        preserve_upstream_encoding_headers(&mut cached_headers, headers);
        return (raw_bytes, cached_headers, false);
    }

    let (stored_bytes, compressed) = maybe_compress(raw_bytes, policy.compression());
    if compressed {
        cached_headers.retain(|(name, _)| {
            !name.eq_ignore_ascii_case("content-encoding")
                && !name.eq_ignore_ascii_case("content-length")
        });
        cached_headers.push(("content-encoding".to_string(), b"gzip".to_vec()));
        cached_headers.push((
            "content-length".to_string(),
            stored_bytes.len().to_string().into_bytes(),
        ));
        for (name, value) in &mut cached_headers {
            if name.eq_ignore_ascii_case("etag") {
                if let Ok(s) = std::str::from_utf8(value) {
                    if let Some(tagged) = tag_gzip_etag_str(s) {
                        *value = tagged.into_bytes();
                    }
                }
            }
        }
        merge_vary_accept_encoding_vec(&mut cached_headers);
    }

    (stored_bytes, cached_headers, compressed)
}

#[cfg(feature = "compression")]
fn maybe_compress(bytes: Bytes, config: CompressionConfig) -> (Bytes, bool) {
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;

    match config.strategy {
        CompressionStrategy::None => (bytes, false),
        CompressionStrategy::Gzip => {
            if bytes.len() < config.min_size {
                return (bytes, false);
            }
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            if encoder.write_all(&bytes).is_err() {
                return (bytes, false);
            }
            match encoder.finish() {
                Ok(data) => (Bytes::from(data), true),
                Err(_) => (bytes, false),
            }
        }
    }
}

#[cfg(not(feature = "compression"))]
fn maybe_compress(bytes: Bytes, _config: CompressionConfig) -> (Bytes, bool) {
    let _ = _config;
    (bytes, false)
}

#[cfg(all(test, feature = "in-memory"))]
mod tests {
    use super::*;
    use crate::backend::CacheEntry;
    use bytes::Bytes;
    use http::{HeaderValue, StatusCode, Version};
    use tokio::task::yield_now;

    fn mock_entry() -> CacheEntry {
        CacheEntry::new(
            StatusCode::OK,
            Version::HTTP_11,
            Vec::new(),
            Bytes::from_static(b"body"),
        )
    }

    #[test]
    fn classify_hit_marks_entry_fresh_when_not_near_expiry() {
        let now = SystemTime::now();
        let hit = CacheRead {
            entry: mock_entry(),
            expires_at: Some(now + Duration::from_secs(10)),
            stale_until: Some(now + Duration::from_secs(20)),
        };

        match classify_hit(hit, Duration::from_secs(5), Duration::from_secs(1)) {
            HitState::Fresh(_) => {}
            other => panic!("expected fresh entry, got {:?}", other),
        }
    }

    #[test]
    fn classify_hit_marks_entry_stale_when_within_refresh_window() {
        let now = SystemTime::now();
        let hit = CacheRead {
            entry: mock_entry(),
            expires_at: Some(now + Duration::from_secs(2)),
            stale_until: Some(now + Duration::from_secs(10)),
        };

        match classify_hit(hit, Duration::from_secs(5), Duration::from_secs(5)) {
            HitState::Stale(_) => {}
            other => panic!("expected stale entry, got {:?}", other),
        }
    }

    #[test]
    fn classify_hit_marks_entry_stale_when_within_stale_window() {
        let now = SystemTime::now();
        let hit = CacheRead {
            entry: mock_entry(),
            expires_at: Some(now - Duration::from_secs(1)),
            stale_until: Some(now + Duration::from_secs(1)),
        };

        match classify_hit(hit, Duration::from_secs(2), Duration::from_secs(0)) {
            HitState::Stale(_) => {}
            other => panic!("expected stale entry, got {:?}", other),
        }
    }

    #[test]
    fn classify_hit_marks_entry_expired_after_stale_window() {
        let now = SystemTime::now();
        let hit = CacheRead {
            entry: mock_entry(),
            expires_at: Some(now - Duration::from_secs(5)),
            stale_until: Some(now - Duration::from_secs(1)),
        };

        match classify_hit(hit, Duration::from_secs(5), Duration::from_secs(0)) {
            HitState::Expired => {}
            other => panic!("expected expired entry, got {:?}", other),
        }
    }

    #[test]
    fn cache_control_disallows_detects_no_cache_directives() {
        let mut headers = HeaderMap::new();
        headers.insert(
            CACHE_CONTROL,
            HeaderValue::from_static("max-age=0, no-cache"),
        );
        assert!(cache_control_disallows(&headers));

        let mut pragma_only = HeaderMap::new();
        pragma_only.insert(PRAGMA, HeaderValue::from_static("no-cache"));
        assert!(cache_control_disallows(&pragma_only));
    }

    #[tokio::test]
    async fn stampede_guard_drop_removes_lock_entry() {
        let locks = Arc::new(DashMap::new());
        let key = "key".to_string();

        match StampedeGuard::acquire_handle(locks.clone(), key.clone()).await {
            StampedeHandle::Primary(guard) => {
                assert!(locks.get(&key).is_some());
                drop(guard);
                yield_now().await;
                assert!(locks.get(&key).is_none());
            }
            StampedeHandle::Secondary(_) => panic!("expected primary guard"),
        }
    }

    #[test]
    fn cache_service_implements_clone() {
        use crate::backend::memory::InMemoryBackend;
        use tower::service_fn;

        // Compile-time check that CacheService implements Clone
        fn assert_clone<T: Clone>(_: &T) {}

        let backend = InMemoryBackend::new(100);
        let layer = CacheLayer::new(backend);
        let service = layer.layer(service_fn(|_req: http::Request<()>| async {
            Ok::<_, std::convert::Infallible>(http::Response::new(()))
        }));

        // This will fail to compile if CacheService doesn't implement Clone
        assert_clone(&service);
    }
}
