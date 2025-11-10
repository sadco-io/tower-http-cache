//! Tower HTTP Cache
//! ==================
//!
//! `tower-http-cache` provides a composable caching layer for Tower-based services with
//! pluggable backends (in-memory, Redis, and more).
//!
//! The crate exposes a single [`CacheLayer`] that can be configured with
//! a variety of policies and storage backends. Most consumers will
//! start from [`CacheLayer::builder`] and choose an in-memory or Redis backend:
//!
//! ```no_run
//! use std::time::Duration;
//! use tower::{Service, ServiceBuilder, ServiceExt};
//! use tower_http_cache::prelude::*;
//!
//! # async fn run() -> Result<(), tower_http_cache::layer::BoxError> {
//! let layer = CacheLayer::builder(InMemoryBackend::new(1_000))
//!     .ttl(Duration::from_secs(30))
//!     .stale_while_revalidate(Duration::from_secs(10))
//!    .build();
//!
//! let mut svc = ServiceBuilder::new()
//!     .layer(layer)
//!     .service(tower::service_fn(|_req| async {
//!         Ok::<_, std::convert::Infallible>(http::Response::new(http_body_util::Full::from("ok")))
//!     }));
//!
//! let response = svc
//!     .ready()
//!     .await?
//!     .call(http::Request::new(()))
//!     .await?;
//! # drop(response);
//! # Ok(())
//! # }
//! ```
//!
//! ## Status
//! The project is under active development. The public API is not yet stabilized.

pub mod admin;
pub mod backend;
pub mod codec;
pub mod error;
pub mod layer;
pub mod logging;
pub mod policy;
pub mod prelude;
pub mod refresh;
pub mod request_id;
pub mod tags;

pub use layer::{CacheLayer, CacheLayerBuilder, KeyExtractor};
pub use logging::{CacheEvent, CacheEventType, MLLoggingConfig};
pub use request_id::RequestId;
pub use tags::{TagIndex, TagPolicy};

#[cfg(feature = "admin-api")]
pub use admin::{AdminConfig, AdminState};
