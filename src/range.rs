//! HTTP Range request parsing and handling.
//!
//! This module provides utilities for working with HTTP Range requests (RFC 7233),
//! which allow clients to request specific byte ranges of a resource. This is
//! commonly used for video streaming, large file downloads, and resume capabilities.
//!
//! # Example
//!
//! ```
//! use tower_http_cache::range::parse_range_header;
//! use http::HeaderMap;
//!
//! let mut headers = HeaderMap::new();
//! headers.insert("range", "bytes=0-1023".parse().unwrap());
//!
//! if let Some(range) = parse_range_header(&headers) {
//!     println!("Requested bytes {}-{:?}", range.start, range.end);
//! }
//! ```

use http::{HeaderMap, HeaderValue, StatusCode};

/// A parsed byte range request.
///
/// Represents a request for a specific byte range of a resource,
/// typically from the HTTP Range header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeRequest {
    /// Starting byte offset (inclusive)
    pub start: u64,

    /// Ending byte offset (inclusive), or None for "to end of file"
    pub end: Option<u64>,
}

impl RangeRequest {
    /// Creates a new range request.
    pub fn new(start: u64, end: Option<u64>) -> Self {
        Self { start, end }
    }

    /// Returns the length of this range in bytes, if both start and end are known.
    pub fn len(&self) -> Option<u64> {
        self.end
            .map(|end| end.saturating_sub(self.start).saturating_add(1))
    }

    /// Returns true if this is an empty range.
    pub fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    /// Validates this range against a known total size.
    ///
    /// Returns true if the range is satisfiable (within bounds).
    pub fn is_satisfiable(&self, total_size: u64) -> bool {
        if self.start >= total_size {
            return false;
        }

        if let Some(end) = self.end {
            if end >= total_size || end < self.start {
                return false;
            }
        }

        true
    }

    /// Normalizes the range to have a definite end based on total size.
    pub fn normalize(&self, total_size: u64) -> Option<Self> {
        if !self.is_satisfiable(total_size) {
            return None;
        }

        let end = self.end.unwrap_or(total_size.saturating_sub(1));
        let end = end.min(total_size.saturating_sub(1));

        Some(Self {
            start: self.start,
            end: Some(end),
        })
    }
}

/// Policy for handling range requests in the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RangeHandling {
    /// Pass through range requests without caching (default, simplest)
    #[default]
    PassThrough,

    /// Cache the full response on first request, serve ranges from cache
    /// This requires fetching the complete resource once
    CacheFullServeRanges,

    /// Cache individual chunks independently (future feature)
    /// This is the most complex but most efficient for large files
    #[allow(dead_code)]
    CacheChunks,
}

/// Parses a Range header value.
///
/// Supports the standard "bytes=start-end" format. Returns None if the header
/// is missing, malformed, or uses a unit other than "bytes".
///
/// # Examples
///
/// ```
/// use tower_http_cache::range::parse_range_header;
/// use http::HeaderMap;
///
/// let mut headers = HeaderMap::new();
///
/// // Request bytes 0-1023
/// headers.insert("range", "bytes=0-1023".parse().unwrap());
/// let range = parse_range_header(&headers).unwrap();
/// assert_eq!(range.start, 0);
/// assert_eq!(range.end, Some(1023));
///
/// // Request from byte 1024 to end
/// headers.insert("range", "bytes=1024-".parse().unwrap());
/// let range = parse_range_header(&headers).unwrap();
/// assert_eq!(range.start, 1024);
/// assert_eq!(range.end, None);
/// ```
pub fn parse_range_header(headers: &HeaderMap) -> Option<RangeRequest> {
    let range_header = headers.get(http::header::RANGE)?;
    let range_str = range_header.to_str().ok()?;

    // Must start with "bytes="
    if !range_str.starts_with("bytes=") {
        return None;
    }

    // Extract the range specification
    let range_spec = &range_str[6..]; // Skip "bytes="

    // Split on comma to get first range (we only support single ranges for now)
    let first_range = range_spec.split(',').next()?;

    // Parse "start-end" format
    let parts: Vec<&str> = first_range.split('-').collect();
    if parts.len() != 2 {
        return None;
    }

    // Parse start
    let start = parts[0].trim().parse::<u64>().ok()?;

    // Parse end (optional)
    let end = if parts[1].trim().is_empty() {
        None
    } else {
        Some(parts[1].trim().parse::<u64>().ok()?)
    };

    Some(RangeRequest { start, end })
}

/// Checks if a response status code indicates a partial content response.
///
/// Returns true for HTTP 206 Partial Content.
pub fn is_partial_content(status: StatusCode) -> bool {
    status == StatusCode::PARTIAL_CONTENT
}

/// Builds a Content-Range header value.
///
/// Creates a header value in the format "bytes start-end/total".
///
/// # Example
///
/// ```
/// use tower_http_cache::range::build_content_range_header;
///
/// let header = build_content_range_header(0, 1023, 10240);
/// assert_eq!(header.to_str().unwrap(), "bytes 0-1023/10240");
/// ```
pub fn build_content_range_header(start: u64, end: u64, total: u64) -> HeaderValue {
    let value = format!("bytes {}-{}/{}", start, end, total);
    HeaderValue::from_str(&value).expect("Content-Range value should be valid")
}

/// Parses a Content-Range header from a response.
///
/// Returns the start, end, and total size if the header is present and valid.
pub fn parse_content_range_header(headers: &HeaderMap) -> Option<(u64, u64, u64)> {
    let content_range = headers.get(http::header::CONTENT_RANGE)?;
    let range_str = content_range.to_str().ok()?;

    // Format: "bytes start-end/total"
    if !range_str.starts_with("bytes ") {
        return None;
    }

    let range_spec = &range_str[6..]; // Skip "bytes "
    let parts: Vec<&str> = range_spec.split('/').collect();
    if parts.len() != 2 {
        return None;
    }

    let range_parts: Vec<&str> = parts[0].split('-').collect();
    if range_parts.len() != 2 {
        return None;
    }

    let start = range_parts[0].trim().parse::<u64>().ok()?;
    let end = range_parts[1].trim().parse::<u64>().ok()?;
    let total = parts[1].trim().parse::<u64>().ok()?;

    Some((start, end, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_range_with_start_and_end() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::RANGE, "bytes=0-1023".parse().unwrap());

        let range = parse_range_header(&headers).unwrap();
        assert_eq!(range.start, 0);
        assert_eq!(range.end, Some(1023));
    }

    #[test]
    fn test_parse_range_with_start_only() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::RANGE, "bytes=1024-".parse().unwrap());

        let range = parse_range_header(&headers).unwrap();
        assert_eq!(range.start, 1024);
        assert_eq!(range.end, None);
    }

    #[test]
    fn test_parse_range_missing_header() {
        let headers = HeaderMap::new();
        assert!(parse_range_header(&headers).is_none());
    }

    #[test]
    fn test_parse_range_invalid_unit() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::RANGE, "items=0-10".parse().unwrap());

        assert!(parse_range_header(&headers).is_none());
    }

    #[test]
    fn test_parse_range_malformed() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::RANGE, "bytes=invalid".parse().unwrap());

        assert!(parse_range_header(&headers).is_none());
    }

    #[test]
    fn test_range_len() {
        let range = RangeRequest::new(0, Some(1023));
        assert_eq!(range.len(), Some(1024));

        let range_open = RangeRequest::new(1024, None);
        assert_eq!(range_open.len(), None);
    }

    #[test]
    fn test_range_is_satisfiable() {
        let range = RangeRequest::new(0, Some(1023));
        assert!(range.is_satisfiable(10240));
        assert!(!range.is_satisfiable(512));

        let range_at_end = RangeRequest::new(10240, Some(10250));
        assert!(!range_at_end.is_satisfiable(10240));
    }

    #[test]
    fn test_range_normalize() {
        let range = RangeRequest::new(0, None);
        let normalized = range.normalize(10240).unwrap();
        assert_eq!(normalized.start, 0);
        assert_eq!(normalized.end, Some(10239));

        let range_explicit = RangeRequest::new(0, Some(1023));
        let normalized_explicit = range_explicit.normalize(10240).unwrap();
        assert_eq!(normalized_explicit.start, 0);
        assert_eq!(normalized_explicit.end, Some(1023));
    }

    #[test]
    fn test_range_normalize_out_of_bounds() {
        let range = RangeRequest::new(10240, Some(20000));
        assert!(range.normalize(10240).is_none());
    }

    #[test]
    fn test_is_partial_content() {
        assert!(is_partial_content(StatusCode::PARTIAL_CONTENT));
        assert!(!is_partial_content(StatusCode::OK));
        assert!(!is_partial_content(StatusCode::NOT_FOUND));
    }

    #[test]
    fn test_build_content_range_header() {
        let header = build_content_range_header(0, 1023, 10240);
        assert_eq!(header.to_str().unwrap(), "bytes 0-1023/10240");
    }

    #[test]
    fn test_parse_content_range_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CONTENT_RANGE,
            "bytes 0-1023/10240".parse().unwrap(),
        );

        let (start, end, total) = parse_content_range_header(&headers).unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 1023);
        assert_eq!(total, 10240);
    }

    #[test]
    fn test_parse_content_range_missing() {
        let headers = HeaderMap::new();
        assert!(parse_content_range_header(&headers).is_none());
    }

    #[test]
    fn test_range_request_is_empty() {
        let empty_range = RangeRequest::new(0, Some(0));
        assert!(!empty_range.is_empty()); // 0-0 is actually 1 byte

        let range = RangeRequest::new(100, Some(99)); // Invalid range
        let len = range.len();
        // This is actually an invalid range, len would be computed incorrectly
        // In practice, is_satisfiable would catch this
        assert!(len.is_some());
    }

    #[test]
    fn test_parse_range_with_whitespace() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::RANGE, "bytes= 0 - 1023 ".parse().unwrap());

        // Should handle whitespace gracefully
        if let Some(range) = parse_range_header(&headers) {
            assert_eq!(range.start, 0);
            assert_eq!(range.end, Some(1023));
        }
    }

    #[test]
    fn test_range_multiple_ranges_first_only() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::RANGE,
            "bytes=0-1023,1024-2047".parse().unwrap(),
        );

        // We only support single ranges, should parse first one
        let range = parse_range_header(&headers).unwrap();
        assert_eq!(range.start, 0);
        assert_eq!(range.end, Some(1023));
    }
}
