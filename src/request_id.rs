//! Request ID infrastructure for request correlation and tracing.
//!
//! This module provides a `RequestId` type for tracking requests across
//! the caching layer and downstream services. Request IDs can be extracted
//! from headers (e.g., X-Request-ID) or generated automatically.

use http::{HeaderValue, Request};
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Unique identifier for tracking a request through the system.
///
/// Request IDs enable correlation of logs, metrics, and traces across
/// different components and services. They can be extracted from incoming
/// headers or generated automatically.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RequestId(String);

impl RequestId {
    /// Generates a new random request ID using UUID v4.
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// Creates a request ID from a string value.
    ///
    /// This is useful when extracting request IDs from headers or
    /// other sources.
    pub fn from_str(s: &str) -> Self {
        Self(s.to_owned())
    }

    /// Attempts to extract a request ID from an HTTP header.
    ///
    /// Returns `None` if the header value is not valid UTF-8.
    pub fn from_header(header: &HeaderValue) -> Option<Self> {
        header.to_str().ok().map(|s| Self(s.to_owned()))
    }

    /// Extracts a request ID from the request headers, or generates a new one.
    ///
    /// Looks for the `X-Request-ID` header first. If not present or invalid,
    /// generates a new random ID.
    pub fn from_request<B>(req: &Request<B>) -> Self {
        req.headers()
            .get("x-request-id")
            .and_then(Self::from_header)
            .unwrap_or_else(Self::new)
    }

    /// Returns the request ID as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the request ID and returns the inner string.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for RequestId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<RequestId> for String {
    fn from(id: RequestId) -> Self {
        id.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::Request;

    #[test]
    fn new_generates_valid_uuid() {
        let id1 = RequestId::new();
        let id2 = RequestId::new();
        assert_ne!(id1, id2);
        assert!(Uuid::parse_str(id1.as_str()).is_ok());
    }

    #[test]
    fn from_str_creates_request_id() {
        let id = RequestId::from_str("custom-id-123");
        assert_eq!(id.as_str(), "custom-id-123");
    }

    #[test]
    fn from_header_extracts_valid_value() {
        let header = HeaderValue::from_static("test-request-id");
        let id = RequestId::from_header(&header).unwrap();
        assert_eq!(id.as_str(), "test-request-id");
    }

    #[test]
    fn from_request_extracts_header() {
        let mut req = Request::builder().body(()).unwrap();
        req.headers_mut()
            .insert("x-request-id", HeaderValue::from_static("header-id"));

        let id = RequestId::from_request(&req);
        assert_eq!(id.as_str(), "header-id");
    }

    #[test]
    fn from_request_generates_when_missing() {
        let req = Request::builder().body(()).unwrap();
        let id = RequestId::from_request(&req);
        assert!(Uuid::parse_str(id.as_str()).is_ok());
    }

    #[test]
    fn display_implementation() {
        let id = RequestId::from_str("test-id");
        assert_eq!(format!("{}", id), "test-id");
    }

    #[test]
    fn string_conversions() {
        let original = "test-id".to_string();
        let id = RequestId::from(original.clone());
        let converted: String = id.into();
        assert_eq!(original, converted);
    }
}
