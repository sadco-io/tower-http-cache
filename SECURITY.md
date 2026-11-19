# Security Policy

## Supported Versions

| Version | Supported | Notes |
|---------|-----------|-------|
| `0.8.x` | Yes | Current release line. Before 1.0, only the latest minor release line receives security fixes. |
| `< 0.8` | No | Upgrade to `0.8.x`. |

## Scope

`tower-http-cache` caches HTTP responses across `CacheLayer`, `InMemoryBackend`, `RedisBackend` (`redis-backend`), `MultiTierBackend`, `ChunkCache`, and the optional `admin-api` handlers, handling untrusted HTTP requests, origin responses, and stored backend payloads (`codec::envelope` and `PostcardCodec`).

- **In scope:** Cache-key confusion or poisoning (including `Accept-Encoding` negotiation and `Vary` handling for `Accept-Encoding`, `Cookie`, `Authorization`, and `*`); violations of configured `Cache-Control` rules (`no-store`, `no-cache`, or `private` when `respect_cache_control` is enabled); failure to strip `Set-Cookie` or hop-by-hop headers from stored entries; cache use for cookie-bearing requests under `CookiePolicy::Bypass`; sharing responses to `Authorization`-bearing requests (or to cookie-bearing requests under `CookiePolicy::RequirePublic`) that lack `public`, `must-revalidate`, or `s-maxage`; panics, memory exhaustion, or incorrect decoding of framed wire-envelope entries (`FORMAT_V1` / `FORMAT_V2` in `codec::envelope` and `PostcardCodec`) from Redis or other backends; and authentication bypasses, missing handler-level enforcement, or timing leaks in `admin-api` authentication (`AdminConfig::validate_token`, `AdminState::authorize`, `admin::routes::validate_auth`, and `admin::routes::handle_*`).
- **Out of scope:** Origin responses that rely on other named `Vary` request headers to separate variants (those headers are not part of the cache key, as documented in `README.md`); deliberately weakened caller policies (such as disabling `respect_cache_control`, setting `CookiePolicy::Ignore`, or disabling `AdminConfig::require_auth`); defects confined to caller-supplied key extractors (`KeyExtractor::custom`) or `CacheCodec` implementations; and vulnerabilities in dependencies such as `moka`, `redis`, `postcard`, or `flate2` (which belong to the upstream dependency, though we will release an update when needed).

## Advisories

For each security fix we publish a GitHub Security Advisory for `tower-http-cache` and request a RustSec
advisory. Fixed versions are listed in the advisory and in [CHANGELOG.md](CHANGELOG.md).

## Reporting a Vulnerability

If you have discovered a security vulnerability in this project, please report
it privately. **Do not disclose it as a public issue.** This gives us time to
work with you to fix the issue before public exposure, reducing the chance that
the exploit will be used before a patch is released.

Please disclose it at [security advisory](https://github.com/sadco-io/tower-http-cache/security/advisories/new).

This project is maintained by a team of volunteers on a reasonable-effort basis.
As such, please give us at least 90 days to work on a fix before public exposure.
