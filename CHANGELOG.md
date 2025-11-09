# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1] - 2025-01-09

### Fixed

- Corrected repository URL in Cargo.toml to point to `sadco-io/tower-http-cache`

## [0.1.0] - 2025-01-09

### Added

- Initial release of `tower-http-cache`
- Drop-in `CacheLayer` for Tower services
- Stampede protection with request deduplication
- Flexible TTL configuration (positive/negative TTL, refresh-before-expiry)
- Stale-while-revalidate support
- Pluggable storage backends:
  - In-memory backend powered by Moka
  - Redis backend with async pooling (optional `redis-backend` feature)
- Policy controls:
  - Min/max body size limits
  - Cache-Control header respect/override
  - Custom method and status code filters
  - Header allowlisting
- Custom cache key extraction
- Optional observability:
  - Metrics counters via `metrics` crate (optional `metrics` feature)
  - Tracing spans (optional `tracing` feature)
- Optional gzip compression (optional `compression` feature)
- Comprehensive test suite
- Benchmark suite with Criterion
- Examples for Axum and Redis integration

[Unreleased]: https://github.com/sadco-io/tower-http-cache/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/sadco-io/tower-http-cache/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/sadco-io/tower-http-cache/releases/tag/v0.1.0
