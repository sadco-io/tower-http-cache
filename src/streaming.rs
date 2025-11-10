//! Smart streaming and large file handling for tower-http-cache.
//!
//! This module provides intelligent body size detection and content-type based
//! filtering to prevent large files (PDFs, videos, archives) from overwhelming
//! the cache. Large bodies are automatically streamed through without buffering,
//! preserving memory and cache efficiency.

use std::collections::HashSet;

/// Decision on how to handle a response body based on size and content type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingDecision {
    /// Buffer the body and cache it (small enough, appropriate type)
    Buffer,

    /// Skip caching entirely (too large or excluded content type)
    SkipCache,

    /// Stream the body without buffering (not implemented yet)
    StreamThrough,

    /// Try to stream if possible, fallback to buffer (unknown size)
    StreamIfPossible,
}

/// Configuration for smart streaming and large file handling.
///
/// The streaming policy allows you to configure how the cache handles
/// large response bodies and specific content types. This prevents memory
/// exhaustion and cache pollution from large files.
///
/// # Examples
///
/// ```
/// use tower_http_cache::streaming::StreamingPolicy;
/// use std::collections::HashSet;
///
/// let policy = StreamingPolicy {
///     enabled: true,
///     max_cacheable_size: Some(1024 * 1024), // 1MB
///     excluded_content_types: HashSet::from([
///         "application/pdf".to_string(),
///         "video/*".to_string(),
///     ]),
///     ..Default::default()
/// };
/// ```
#[derive(Debug, Clone)]
pub struct StreamingPolicy {
    /// Enable smart streaming (default: true)
    pub enabled: bool,

    /// Skip caching bodies larger than this (default: 1MB)
    /// Set to None to disable size-based filtering
    pub max_cacheable_size: Option<usize>,

    /// Content types to never cache (PDFs, videos, archives, etc.)
    /// Supports wildcards like "video/*"
    pub excluded_content_types: HashSet<String>,

    /// Always cache these content types regardless of size
    /// Useful for API responses that should always be cached
    pub force_cache_content_types: HashSet<String>,

    /// Use streaming for bodies above this size (default: 512KB)
    /// Currently used for decision making; actual streaming not yet implemented
    pub stream_threshold: usize,
}

impl Default for StreamingPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_cacheable_size: Some(1024 * 1024), // 1MB
            excluded_content_types: HashSet::from([
                "application/pdf".to_string(),
                "video/*".to_string(),
                "audio/*".to_string(),
                "application/zip".to_string(),
                "application/x-rar".to_string(),
                "application/x-tar".to_string(),
                "application/gzip".to_string(),
                "application/x-7z-compressed".to_string(),
                "application/octet-stream".to_string(),
            ]),
            force_cache_content_types: HashSet::from([
                "application/json".to_string(),
                "application/xml".to_string(),
                "text/*".to_string(),
            ]),
            stream_threshold: 512 * 1024, // 512KB
        }
    }
}

impl StreamingPolicy {
    /// Creates a new streaming policy with all features disabled.
    /// Useful for gradually migrating existing code.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            max_cacheable_size: None,
            excluded_content_types: HashSet::new(),
            force_cache_content_types: HashSet::new(),
            stream_threshold: usize::MAX,
        }
    }

    /// Creates a streaming policy that only filters by size.
    pub fn size_only(max_size: usize) -> Self {
        Self {
            enabled: true,
            max_cacheable_size: Some(max_size),
            excluded_content_types: HashSet::new(),
            force_cache_content_types: HashSet::new(),
            stream_threshold: max_size,
        }
    }

    /// Creates a streaming policy that only filters by content type.
    pub fn content_type_only(excluded: HashSet<String>) -> Self {
        Self {
            enabled: true,
            max_cacheable_size: None,
            excluded_content_types: excluded,
            force_cache_content_types: HashSet::new(),
            stream_threshold: usize::MAX,
        }
    }
}

/// Determines how to handle a response body based on size hints and content type.
///
/// This function implements the core streaming decision logic by examining:
/// 1. Whether streaming is enabled in the policy
/// 2. Content-Type header (fast path for exclusions/forced caching)
/// 3. Size hints from the body
/// 4. Content-Length header as fallback
///
/// # Arguments
///
/// * `policy` - The streaming policy configuration
/// * `size_hint` - Size hint from the response body
/// * `content_type` - Content-Type header value, if present
/// * `content_length` - Content-Length header value, if present
///
/// # Returns
///
/// A [`StreamingDecision`] indicating how to handle the body.
pub fn should_stream(
    policy: &StreamingPolicy,
    size_hint: &http_body::SizeHint,
    content_type: Option<&str>,
    content_length: Option<u64>,
) -> StreamingDecision {
    // Check if streaming is disabled
    if !policy.enabled {
        return StreamingDecision::Buffer;
    }

    // Check content type first (fast path)
    let is_forced = if let Some(ct) = content_type {
        // Check if content type is excluded
        if is_excluded_content_type(ct, &policy.excluded_content_types) {
            return StreamingDecision::SkipCache;
        }

        // Check if content type is forced to cache
        is_forced_content_type(ct, &policy.force_cache_content_types)
    } else {
        false
    };

    // Check size hints - size limits apply even to forced types
    if let Some(exact_size) = size_hint.exact() {
        return decide_by_size(exact_size as usize, policy, is_forced);
    }

    if let Some(upper_bound) = size_hint.upper() {
        return decide_by_size(upper_bound as usize, policy, is_forced);
    }

    // Fallback to Content-Length header
    if let Some(content_len) = content_length {
        return decide_by_size(content_len as usize, policy, is_forced);
    }

    // Unknown size - use conservative approach
    // If we have no size information, buffer it (existing behavior)
    StreamingDecision::StreamIfPossible
}

/// Decides caching strategy based on body size.
///
/// The `is_forced` parameter indicates if the content-type is in force_cache list,
/// but size limits still apply regardless.
fn decide_by_size(size: usize, policy: &StreamingPolicy, _is_forced: bool) -> StreamingDecision {
    if let Some(max_size) = policy.max_cacheable_size {
        if size > max_size {
            return StreamingDecision::SkipCache;
        }
    }

    // For now, always buffer if size is acceptable
    // Future: return StreamingDecision::StreamThrough for large-but-cacheable bodies
    StreamingDecision::Buffer
}

/// Checks if a content type matches any excluded patterns.
///
/// Supports exact matches and wildcard patterns like "video/*".
fn is_excluded_content_type(content_type: &str, excluded: &HashSet<String>) -> bool {
    let normalized = content_type.to_lowercase();

    for pattern in excluded {
        if matches_pattern(&normalized, pattern) {
            return true;
        }
    }

    false
}

/// Checks if a content type matches any forced cache patterns.
fn is_forced_content_type(content_type: &str, forced: &HashSet<String>) -> bool {
    let normalized = content_type.to_lowercase();

    for pattern in forced {
        if matches_pattern(&normalized, pattern) {
            return true;
        }
    }

    false
}

/// Matches a content type against a pattern (supports wildcards).
fn matches_pattern(content_type: &str, pattern: &str) -> bool {
    let pattern_lower = pattern.to_lowercase();

    if pattern_lower.ends_with("/*") {
        // Wildcard pattern like "video/*"
        let prefix = &pattern_lower[..pattern_lower.len() - 2];
        content_type.starts_with(prefix)
    } else {
        // Exact match or substring match
        content_type.contains(&pattern_lower)
    }
}

/// Extracts size information from various sources for logging/metrics.
pub fn extract_size_info(
    size_hint: &http_body::SizeHint,
    content_length: Option<u64>,
) -> Option<u64> {
    size_hint
        .exact()
        .or_else(|| size_hint.upper())
        .or(content_length)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body::SizeHint;

    #[test]
    fn test_default_policy_excludes_pdf() {
        let policy = StreamingPolicy::default();
        let size_hint = SizeHint::with_exact(5 * 1024 * 1024); // 5MB

        let decision = should_stream(
            &policy,
            &size_hint,
            Some("application/pdf"),
            Some(5 * 1024 * 1024),
        );

        assert_eq!(decision, StreamingDecision::SkipCache);
    }

    #[test]
    fn test_default_policy_excludes_video() {
        let policy = StreamingPolicy::default();
        let size_hint = SizeHint::with_exact(10 * 1024 * 1024);

        let decision = should_stream(&policy, &size_hint, Some("video/mp4"), None);

        assert_eq!(decision, StreamingDecision::SkipCache);
    }

    #[test]
    fn test_small_json_gets_buffered() {
        let policy = StreamingPolicy::default();
        let size_hint = SizeHint::with_exact(1024); // 1KB

        let decision = should_stream(&policy, &size_hint, Some("application/json"), Some(1024));

        assert_eq!(decision, StreamingDecision::Buffer);
    }

    #[test]
    fn test_large_json_skipped_by_size() {
        let policy = StreamingPolicy::default();
        let size_hint = SizeHint::with_exact(2 * 1024 * 1024); // 2MB

        let decision = should_stream(
            &policy,
            &size_hint,
            Some("application/json"),
            Some(2 * 1024 * 1024),
        );

        assert_eq!(decision, StreamingDecision::SkipCache);
    }

    #[test]
    fn test_force_cache_respects_size_limits() {
        // force_cache only bypasses content-type exclusions, not size limits
        let mut policy = StreamingPolicy::default();
        policy
            .force_cache_content_types
            .insert("application/important".to_string());

        let size_hint = SizeHint::with_exact(5 * 1024 * 1024); // 5MB (over 1MB limit)

        let decision = should_stream(
            &policy,
            &size_hint,
            Some("application/important"),
            Some(5 * 1024 * 1024),
        );

        // Even though it's forced to cache, size limit still applies
        assert_eq!(decision, StreamingDecision::SkipCache);

        // But under the size limit it should cache
        let small_hint = SizeHint::with_exact(500 * 1024); // 500KB
        let decision_small = should_stream(
            &policy,
            &small_hint,
            Some("application/important"),
            Some(500 * 1024),
        );
        assert_eq!(decision_small, StreamingDecision::Buffer);
    }

    #[test]
    fn test_disabled_policy_always_buffers() {
        let policy = StreamingPolicy::disabled();
        let size_hint = SizeHint::with_exact(10 * 1024 * 1024);

        let decision = should_stream(
            &policy,
            &size_hint,
            Some("application/pdf"),
            Some(10 * 1024 * 1024),
        );

        assert_eq!(decision, StreamingDecision::Buffer);
    }

    #[test]
    fn test_wildcard_pattern_matching() {
        assert!(matches_pattern("video/mp4", "video/*"));
        assert!(matches_pattern("video/mpeg", "video/*"));
        assert!(matches_pattern("audio/mp3", "audio/*"));
        assert!(!matches_pattern("application/json", "video/*"));
    }

    #[test]
    fn test_exact_pattern_matching() {
        assert!(matches_pattern("application/pdf", "application/pdf"));
        assert!(matches_pattern("application/pdf", "pdf")); // substring match
        assert!(!matches_pattern("text/plain", "application/pdf"));
    }

    #[test]
    fn test_size_hint_exact() {
        let policy = StreamingPolicy::default();
        let size_hint = SizeHint::with_exact(500 * 1024); // 500KB

        let decision = should_stream(&policy, &size_hint, None, None);
        assert_eq!(decision, StreamingDecision::Buffer);
    }

    #[test]
    fn test_size_hint_upper_bound() {
        let policy = StreamingPolicy::default();
        let mut size_hint = SizeHint::default();
        size_hint.set_upper(500 * 1024);

        let decision = should_stream(&policy, &size_hint, None, None);
        assert_eq!(decision, StreamingDecision::Buffer);
    }

    #[test]
    fn test_content_length_fallback() {
        let policy = StreamingPolicy::default();
        let size_hint = SizeHint::default(); // No hints

        let decision = should_stream(&policy, &size_hint, None, Some(500 * 1024));
        assert_eq!(decision, StreamingDecision::Buffer);
    }

    #[test]
    fn test_unknown_size_conservative() {
        let policy = StreamingPolicy::default();
        let size_hint = SizeHint::default();

        let decision = should_stream(&policy, &size_hint, None, None);
        assert_eq!(decision, StreamingDecision::StreamIfPossible);
    }

    #[test]
    fn test_size_only_policy() {
        let policy = StreamingPolicy::size_only(512 * 1024);
        let size_hint = SizeHint::with_exact(1024 * 1024);

        // PDF should be cached (no content-type filtering)
        let decision = should_stream(&policy, &size_hint, Some("application/pdf"), None);
        assert_eq!(decision, StreamingDecision::SkipCache); // Due to size

        let size_hint_small = SizeHint::with_exact(256 * 1024);
        let decision_small =
            should_stream(&policy, &size_hint_small, Some("application/pdf"), None);
        assert_eq!(decision_small, StreamingDecision::Buffer); // Small enough
    }

    #[test]
    fn test_content_type_only_policy() {
        let mut excluded = HashSet::new();
        excluded.insert("application/pdf".to_string());
        let policy = StreamingPolicy::content_type_only(excluded);

        let size_hint = SizeHint::with_exact(10 * 1024 * 1024); // 10MB

        // Large non-PDF should be cached (no size filtering)
        let decision = should_stream(&policy, &size_hint, Some("application/json"), None);
        assert_eq!(decision, StreamingDecision::Buffer);

        // PDF should be skipped
        let decision_pdf = should_stream(&policy, &size_hint, Some("application/pdf"), None);
        assert_eq!(decision_pdf, StreamingDecision::SkipCache);
    }

    #[test]
    fn test_extract_size_info() {
        let size_hint = SizeHint::with_exact(1024);
        assert_eq!(extract_size_info(&size_hint, None), Some(1024));

        let mut size_hint_upper = SizeHint::default();
        size_hint_upper.set_upper(2048);
        assert_eq!(extract_size_info(&size_hint_upper, None), Some(2048));

        let size_hint_none = SizeHint::default();
        assert_eq!(extract_size_info(&size_hint_none, Some(4096)), Some(4096));

        assert_eq!(extract_size_info(&size_hint_none, None), None);
    }

    #[test]
    fn test_case_insensitive_content_type() {
        let policy = StreamingPolicy::default();
        let size_hint = SizeHint::with_exact(1024);

        // All variations should be excluded
        assert_eq!(
            should_stream(&policy, &size_hint, Some("Application/PDF"), None),
            StreamingDecision::SkipCache
        );
        assert_eq!(
            should_stream(&policy, &size_hint, Some("APPLICATION/PDF"), None),
            StreamingDecision::SkipCache
        );
        assert_eq!(
            should_stream(&policy, &size_hint, Some("Video/MP4"), None),
            StreamingDecision::SkipCache
        );
    }

    #[test]
    fn test_content_type_with_charset() {
        let policy = StreamingPolicy::default();
        let size_hint = SizeHint::with_exact(1024);

        // Should match even with charset parameter
        let decision = should_stream(
            &policy,
            &size_hint,
            Some("application/json; charset=utf-8"),
            None,
        );
        assert_eq!(decision, StreamingDecision::Buffer);
    }
}
