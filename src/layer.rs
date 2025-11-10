use std::error::Error as StdError;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use futures_util::future::BoxFuture;
use http::header::{CACHE_CONTROL, PRAGMA};
use http::{HeaderMap, Method, Request, Response, Uri};
use http_body::Body;
use http_body_util::{BodyExt, Full};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tower::{Layer, Service, ServiceExt};

#[cfg(feature = "metrics")]
use metrics::{counter, histogram};

use crate::backend::memory::InMemoryBackend;
use crate::backend::{CacheBackend, CacheEntry, CacheRead};
#[cfg(feature = "compression")]
use crate::policy::CompressionStrategy;
use crate::policy::{CachePolicy, CompressionConfig};
use crate::refresh::{AutoRefreshConfig, RefreshCallback, RefreshManager, RefreshMetadata};

pub type BoxError = Box<dyn StdError + Send + Sync>;

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
    /// Builds an extractor that uses `method + path + query` for GET/HEAD requests.
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

    pub fn key_extractor(mut self, extractor: KeyExtractor) -> Self {
        self.key_extractor = extractor;
        self
    }

    /// Enables auto-refresh functionality with the provided configuration.
    ///
    /// When enabled, frequently accessed cache entries will be proactively
    /// refreshed before they expire, reducing cache misses and latency.
    pub fn auto_refresh(mut self, config: AutoRefreshConfig) -> Self {
        self.auto_refresh_config = Some(config);
        self
    }

    pub fn build(self) -> CacheLayer<B> {
        let refresh_manager = self
            .auto_refresh_config
            .filter(|cfg| cfg.enabled)
            .map(|cfg| Arc::new(RefreshManager::new(cfg)));

        CacheLayer {
            backend: self.backend,
            policy: self.policy,
            key_extractor: self.key_extractor,
            locks: Arc::new(DashMap::new()),
            refresh_manager,
        }
    }
}

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

    /// Manually initialize the auto-refresh manager with a service instance.
    ///
    /// This should be called after constructing the service to start the background
    /// refresh task. This is only necessary if auto-refresh is enabled.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let layer = CacheLayer::builder(backend)
    ///     .auto_refresh(config)
    ///     .build();
    ///
    /// layer.init_auto_refresh(my_service.clone()).await?;
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
                self.key_extractor.clone(),
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
        }
    }
}

impl<B> Drop for CacheLayer<B> {
    fn drop(&mut self) {
        // Trigger graceful shutdown of refresh manager
        // We use tokio::spawn to avoid blocking in Drop
        if let Some(manager) = &self.refresh_manager {
            let manager = manager.clone();
            // Best-effort shutdown - spawn detached task
            // Note: We cannot guarantee execution in Drop, but we try our best
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    manager.shutdown().await;
                });
            }
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
}

/// Implementation of RefreshCallback for CacheService.
struct CacheRefreshCallback<S, B> {
    inner: S,
    backend: B,
    policy: CachePolicy,
}

impl<S, B> CacheRefreshCallback<S, B> {
    fn new(inner: S, backend: B, policy: CachePolicy, _key_extractor: KeyExtractor) -> Self {
        Self {
            inner,
            backend,
            policy,
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

        Box::pin(async move {
            #[cfg(feature = "tracing")]
            tracing::debug!(key = %key, uri = %metadata.uri, "Auto-refresh triggered");

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

            // Check if we should cache this response
            let body_too_large = policy
                .max_body_size()
                .is_some_and(|max| cache_bytes.len() > max);
            let body_too_small = policy
                .min_body_size()
                .is_some_and(|min| cache_bytes.len() < min);

            if body_too_large || body_too_small {
                return Ok(()); // Successfully refreshed but not stored
            }

            // Store the refreshed entry
            if let Some(ttl) = policy.ttl_for(parts.status) {
                if !ttl.is_zero() {
                    let stale_for = policy.stale_while_revalidate();
                    let headers_to_cache = policy.headers_to_cache(&parts.headers);
                    let (compressed_bytes, _compressed) =
                        maybe_compress(cache_bytes, policy.compression());

                    let entry = CacheEntry::new(
                        parts.status,
                        parts.version,
                        headers_to_cache,
                        compressed_bytes,
                    );

                    if let Err(_err) = backend.set(key.clone(), entry, ttl, stale_for).await {
                        #[cfg(feature = "tracing")]
                        tracing::error!(key = %key, "Failed to store refreshed entry");
                        return Err("Failed to store entry".into());
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
    type Response = Response<Full<Bytes>>;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let method = req.method().clone();
        let uri = req.uri().clone();
        let should_cache_method = self.policy.should_cache_method(&method);
        let request_bypass =
            self.policy.respect_cache_control() && cache_control_disallows(req.headers());
        let key = if should_cache_method && !request_bypass {
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

        // Prepare refresh metadata if auto-refresh is enabled
        let refresh_metadata = if refresh_manager.is_some() && key.is_some() {
            Some(RefreshMetadata::from_request(&req))
        } else {
            None
        };

        Box::pin(async move {
            #[cfg(feature = "tracing")]
            tracing::debug!(method = %method, uri = %uri, "cache_call");

            let mut stale_entry: Option<CacheEntry> = None;
            if let Some(ref key_ref) = key {
                if let Ok(Some(hit)) = backend.get(key_ref).await {
                    match classify_hit(hit, stale_window, refresh_before) {
                        HitState::Fresh(entry) => {
                            #[cfg(feature = "metrics")]
                            counter!("tower_http_cache.hit").increment(1);

                            // Record hit for auto-refresh tracking
                            if let Some(ref manager) = refresh_manager {
                                manager.tracker().record_hit(key_ref);
                            }

                            return Ok(entry.into_response());
                        }
                        HitState::Stale(entry) => {
                            #[cfg(feature = "metrics")]
                            counter!("tower_http_cache.stale_hit").increment(1);

                            // Record hit for auto-refresh tracking
                            if let Some(ref manager) = refresh_manager {
                                manager.tracker().record_hit(key_ref);
                            }

                            stale_entry = Some(entry);
                        }
                        HitState::Expired => {}
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
                            return Ok(entry.into_response());
                        }

                        let secondary_guard = lock.lock_owned().await;
                        drop(secondary_guard);

                        if let Ok(Some(hit)) = backend.get(key_ref).await {
                            match classify_hit(hit, stale_window, refresh_before) {
                                HitState::Fresh(entry) => {
                                    #[cfg(feature = "metrics")]
                                    counter!("tower_http_cache.hit_after_wait").increment(1);
                                    return Ok(entry.into_response());
                                }
                                HitState::Stale(entry) => {
                                    #[cfg(feature = "metrics")]
                                    counter!("tower_http_cache.stale_served").increment(1);
                                    return Ok(entry.into_response());
                                }
                                HitState::Expired => {}
                            }
                        }

                        if let StampedeHandle::Primary(guard) =
                            StampedeGuard::acquire_handle(locks.clone(), key_ref.clone()).await
                        {
                            primary_guard = Some(guard);
                        }
                    }
                }
            }

            #[cfg(feature = "metrics")]
            counter!("tower_http_cache.miss").increment(1);

            #[cfg(feature = "metrics")]
            let start = std::time::Instant::now();
            let service = inner;
            let response = service.oneshot(req).await.map_err(|err| err.into())?;
            #[cfg(feature = "metrics")]
            histogram!("tower_http_cache.backend_latency").record(start.elapsed().as_secs_f64());

            let (parts, body) = response.into_parts();

            let streaming = body.size_hint().upper().is_none();
            if streaming && !policy.allow_streaming_bodies() {
                #[cfg(feature = "metrics")]
                counter!("tower_http_cache.streaming_skip").increment(1);
            }

            let collected = BodyExt::collect(body).await.map_err(|err| err.into())?;
            let cache_bytes = collected.to_bytes();
            let response_bytes = cache_bytes.clone();

            let cache_control_block =
                policy.respect_cache_control() && cache_control_disallows(&parts.headers);
            let body_too_large = policy
                .max_body_size()
                .is_some_and(|max| cache_bytes.len() > max);
            let body_too_small = policy
                .min_body_size()
                .is_some_and(|min| cache_bytes.len() < min);

            let should_store = key.is_some()
                && !cache_control_block
                && !body_too_large
                && !body_too_small
                && (policy.allow_streaming_bodies() || !streaming);

            let headers_to_cache = if should_store {
                Some(policy.headers_to_cache(&parts.headers))
            } else {
                None
            };

            let version = parts.version;
            let status = parts.status;

            if should_store {
                if let Some(key_ref) = &key {
                    if let Some(ttl) = policy.ttl_for(status) {
                        if !ttl.is_zero() {
                            let stale_for = policy.stale_while_revalidate();
                            let (compressed_bytes, compressed) =
                                maybe_compress(cache_bytes.clone(), policy.compression());
                            if compressed {
                                #[cfg(feature = "metrics")]
                                counter!("tower_http_cache.compressed").increment(1);
                            }
                            let entry = CacheEntry::new(
                                status,
                                version,
                                headers_to_cache.unwrap(),
                                compressed_bytes,
                            );
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

                                // Store refresh metadata if auto-refresh is enabled
                                if let (Some(ref manager), Some(metadata)) =
                                    (&refresh_manager, refresh_metadata)
                                {
                                    manager.store_metadata(key_ref.clone(), metadata);
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

            Ok(Response::from_parts(parts, Full::from(response_bytes)))
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

#[cfg(feature = "compression")]
fn maybe_compress(bytes: Bytes, config: CompressionConfig) -> (Bytes, bool) {
    use flate2::{write::GzEncoder, Compression};
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

#[cfg(test)]
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
