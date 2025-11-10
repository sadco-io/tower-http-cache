use http::{HeaderMap, Method, StatusCode};
use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use crate::logging::MLLoggingConfig;
use crate::streaming::StreamingPolicy;
use crate::tags::TagPolicy;

/// Type alias for the method predicate function
type MethodPredicateFn = Arc<dyn Fn(&Method) -> bool + Send + Sync>;

/// Type alias for tag extractor function
type TagExtractorFn = Arc<dyn Fn(&Method, &http::Uri) -> Vec<String> + Send + Sync>;

/// Runtime cache policy shared by the layer and backend.
///
/// Policies define how long responses stay in cache, which headers are
/// persisted, which status codes are cacheable, and much more. Policies are
/// cheap to clone and are immutable—the `with_*` builder helpers return new
/// copies with the requested change.
#[derive(Clone)]
pub struct CachePolicy {
    ttl: Duration,
    negative_ttl: Duration,
    stale_while_revalidate: Duration,
    refresh_before: Duration,
    max_body_size: Option<usize>,
    min_body_size: Option<usize>,
    cache_statuses: HashSet<u16>,
    respect_cache_control: bool,
    method_predicate: Option<MethodPredicateFn>,
    header_allowlist: Option<HashSet<String>>,
    allow_streaming_bodies: bool,
    compression: CompressionConfig,
    ml_logging: MLLoggingConfig,
    tag_policy: TagPolicy,
    tag_extractor: Option<TagExtractorFn>,
    streaming_policy: StreamingPolicy,
}

/// Strategy for compressing cached payloads.
#[derive(Clone, Copy, Debug)]
pub enum CompressionStrategy {
    None,
    Gzip,
}

/// Compression configuration attached to a [`CachePolicy`].
#[derive(Clone, Copy, Debug)]
pub struct CompressionConfig {
    pub strategy: CompressionStrategy,
    pub min_size: usize,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            strategy: CompressionStrategy::None,
            min_size: 1024,
        }
    }
}

impl fmt::Debug for CachePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachePolicy")
            .field("ttl", &self.ttl)
            .field("negative_ttl", &self.negative_ttl)
            .field("stale_while_revalidate", &self.stale_while_revalidate)
            .field("refresh_before", &self.refresh_before)
            .field("max_body_size", &self.max_body_size)
            .field("min_body_size", &self.min_body_size)
            .field("cache_statuses", &self.cache_statuses)
            .field("respect_cache_control", &self.respect_cache_control)
            .field(
                "header_allowlist",
                &self
                    .header_allowlist
                    .as_ref()
                    .map(|set| set.iter().collect::<Vec<_>>()),
            )
            .field("allow_streaming_bodies", &self.allow_streaming_bodies)
            .field("compression", &self.compression)
            .finish()
    }
}

impl CachePolicy {
    /// Builds a new policy with explicit TTL settings and cacheable statuses.
    pub fn new(
        ttl: Duration,
        negative_ttl: Duration,
        statuses: impl IntoIterator<Item = u16>,
    ) -> Self {
        Self {
            ttl,
            negative_ttl,
            stale_while_revalidate: Duration::from_secs(0),
            refresh_before: Duration::from_secs(0),
            max_body_size: None,
            min_body_size: None,
            cache_statuses: statuses.into_iter().collect(),
            respect_cache_control: true,
            method_predicate: None,
            header_allowlist: None,
            allow_streaming_bodies: false,
            compression: CompressionConfig::default(),
            ml_logging: MLLoggingConfig::default(),
            tag_policy: TagPolicy::default(),
            tag_extractor: None,
            streaming_policy: StreamingPolicy::default(),
        }
    }

    /// Returns the TTL for the given HTTP status code if it should be cached.
    pub fn ttl_for(&self, status: StatusCode) -> Option<Duration> {
        if self.cache_statuses.contains(&status.as_u16()) {
            Some(self.ttl)
        } else if status.is_client_error() && !self.negative_ttl.is_zero() {
            Some(self.negative_ttl)
        } else {
            None
        }
    }

    /// Determines whether the request method is cacheable.
    pub fn should_cache_method(&self, method: &Method) -> bool {
        if let Some(predicate) = &self.method_predicate {
            predicate(method)
        } else {
            matches!(method, &Method::GET | &Method::HEAD)
        }
    }

    /// Returns whether `Cache-Control`/`Pragma` headers on requests are honored.
    pub fn respect_cache_control(&self) -> bool {
        self.respect_cache_control
    }

    pub fn max_body_size(&self) -> Option<usize> {
        self.max_body_size
    }

    pub fn min_body_size(&self) -> Option<usize> {
        self.min_body_size
    }

    pub fn allow_streaming_bodies(&self) -> bool {
        self.allow_streaming_bodies
    }

    pub fn compression(&self) -> CompressionConfig {
        self.compression
    }

    pub fn refresh_before(&self) -> Duration {
        self.refresh_before
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    pub fn negative_ttl(&self) -> Duration {
        self.negative_ttl
    }

    pub fn stale_while_revalidate(&self) -> Duration {
        self.stale_while_revalidate
    }

    pub fn ml_logging(&self) -> &MLLoggingConfig {
        &self.ml_logging
    }

    pub fn tag_policy(&self) -> &TagPolicy {
        &self.tag_policy
    }

    pub fn streaming_policy(&self) -> &StreamingPolicy {
        &self.streaming_policy
    }

    /// Extracts tags for a request using the configured tag extractor.
    pub fn extract_tags(&self, method: &Method, uri: &http::Uri) -> Vec<String> {
        if !self.tag_policy.enabled {
            return Vec::new();
        }

        if let Some(ref extractor) = self.tag_extractor {
            let tags = extractor(method, uri);
            self.tag_policy.validate_tags(tags)
        } else {
            Vec::new()
        }
    }

    /// Returns the headers that should be cached based on the allowlist.
    pub fn headers_to_cache(&self, headers: &HeaderMap) -> Vec<(String, Vec<u8>)> {
        match &self.header_allowlist {
            Some(allowlist) => headers
                .iter()
                .filter(|(name, _)| allowlist.contains(&name.as_str().to_ascii_lowercase()))
                .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
                .collect(),
            None => headers
                .iter()
                .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
                .collect(),
        }
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn with_negative_ttl(mut self, ttl: Duration) -> Self {
        self.negative_ttl = ttl;
        self
    }

    pub fn with_statuses(mut self, statuses: impl IntoIterator<Item = u16>) -> Self {
        self.cache_statuses = statuses.into_iter().collect();
        self
    }

    pub fn with_stale_while_revalidate(mut self, duration: Duration) -> Self {
        self.stale_while_revalidate = duration;
        self
    }

    pub fn with_refresh_before(mut self, duration: Duration) -> Self {
        self.refresh_before = duration;
        self
    }

    pub fn with_max_body_size(mut self, size: Option<usize>) -> Self {
        self.max_body_size = size;
        self
    }

    pub fn with_min_body_size(mut self, size: Option<usize>) -> Self {
        self.min_body_size = size;
        self
    }

    pub fn with_allow_streaming_bodies(mut self, allow: bool) -> Self {
        self.allow_streaming_bodies = allow;
        self
    }

    pub fn with_compression(mut self, config: CompressionConfig) -> Self {
        self.compression = config;
        self
    }

    pub fn with_respect_cache_control(mut self, enabled: bool) -> Self {
        self.respect_cache_control = enabled;
        self
    }

    pub fn with_method_predicate<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&Method) -> bool + Send + Sync + 'static,
    {
        self.method_predicate = Some(Arc::new(predicate));
        self
    }

    pub fn with_header_allowlist<I, S>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.header_allowlist = Some(
            headers
                .into_iter()
                .map(|h| h.into().to_ascii_lowercase())
                .collect(),
        );
        self
    }

    pub fn with_ml_logging(mut self, config: MLLoggingConfig) -> Self {
        self.ml_logging = config;
        self
    }

    pub fn with_tag_policy(mut self, policy: TagPolicy) -> Self {
        self.tag_policy = policy;
        self
    }

    pub fn with_tag_extractor<F>(mut self, extractor: F) -> Self
    where
        F: Fn(&Method, &http::Uri) -> Vec<String> + Send + Sync + 'static,
    {
        self.tag_extractor = Some(Arc::new(extractor));
        self
    }

    pub fn with_streaming_policy(mut self, policy: StreamingPolicy) -> Self {
        self.streaming_policy = policy;
        self
    }
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(60),
            negative_ttl: Duration::from_secs(5),
            stale_while_revalidate: Duration::from_secs(0),
            refresh_before: Duration::from_secs(0),
            max_body_size: None,
            min_body_size: None,
            cache_statuses: HashSet::from([200, 203, 300, 301, 404]),
            respect_cache_control: true,
            method_predicate: None,
            header_allowlist: None,
            allow_streaming_bodies: false,
            compression: CompressionConfig::default(),
            ml_logging: MLLoggingConfig::default(),
            tag_policy: TagPolicy::default(),
            tag_extractor: None,
            streaming_policy: StreamingPolicy::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
    use std::collections::HashSet;

    #[test]
    fn ttl_for_prefers_primary_ttl_for_cacheable_status() {
        let policy = CachePolicy::default().with_ttl(Duration::from_secs(123));
        assert_eq!(
            policy.ttl_for(StatusCode::OK),
            Some(Duration::from_secs(123))
        );
    }

    #[test]
    fn ttl_for_uses_negative_ttl_for_client_error() {
        let policy = CachePolicy::default().with_negative_ttl(Duration::from_secs(9));
        assert_eq!(
            policy.ttl_for(StatusCode::BAD_REQUEST),
            Some(Duration::from_secs(9))
        );
        assert_eq!(policy.ttl_for(StatusCode::INTERNAL_SERVER_ERROR), None);
    }

    #[test]
    fn headers_to_cache_respects_allowlist() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("text/plain"),
        );
        headers.insert(
            HeaderName::from_static("x-cacheable"),
            HeaderValue::from_static("yes"),
        );

        let policy = CachePolicy::default().with_header_allowlist(["content-type"]);
        let cached = policy.headers_to_cache(&headers);

        let expected = vec![("content-type".to_owned(), b"text/plain".to_vec())];
        assert_eq!(cached, expected);
    }

    #[test]
    fn method_predicate_overrides_default_behavior() {
        let policy = CachePolicy::default().with_method_predicate(|method| method == Method::POST);
        assert!(!policy.should_cache_method(&Method::GET));
        assert!(policy.should_cache_method(&Method::POST));
    }

    #[test]
    fn with_statuses_updates_allowlist() {
        let policy = CachePolicy::default().with_statuses([201, 202]);
        assert_eq!(policy.ttl_for(StatusCode::CREATED), Some(policy.ttl()));
        assert_eq!(policy.ttl_for(StatusCode::OK), None);
        assert_eq!(policy.cache_statuses, HashSet::from([201_u16, 202_u16]));
    }
}
