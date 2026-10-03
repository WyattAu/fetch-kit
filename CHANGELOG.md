# Changelog

All notable changes to this project are documented here. Format: [Keep a
Changelog](https://keepachangelog.com/) — versions follow [semver](https://semver.org).

## [Unreleased]

## [0.2.0] - 2026-10-03

### Added

- **Native middleware stack** (breaking): `middleware::Middleware` trait —
  `handle(req, extensions, next: Next<'_>) -> Result<Response>` with the
  same shape as `reqwest-middleware` 0.5, so migrating an existing
  middleware is an import-path swap. Includes `ClientWithMiddleware`
  (clone-able, `Debug`), a middleware-level `ClientBuilder`
  (`with`/`with_middleware`, **first-registered = outermost**), a
  middleware-level `RequestBuilder`, and `Next::run` continuations.
  Documented ordering semantics live in the crate and module docs (onion
  model with a diagram) and are proven by
  `tests/middleware.rs::first_registered_middleware_is_outermost_onion_order`.
- **Bundled middleware** (each feature-gated):
  - `RetryMiddleware` (feature `retry`, **default-on**): delegates
    backoff timing to the estate `loop-retry` crate (exponential, ×2,
    ≤10% jitter) — `RetryConfig` is re-exported. Retries transport
    timeouts/connect errors and transient statuses (408, 429, 5xx except
    501); middleware errors are never retried, so downstream
    short-circuits (e.g. an open breaker) surface immediately. Exhausted
    retryable statuses return the final response unchanged.
  - `ThrottleMiddleware` (feature `throttle`): per-authority
    continuous-refill token bucket — sustained throughput is rate-bounded
    (property-tested over 300 proptest cases), bursts up to
    `burst_capacity`.
  - `TimeoutMiddleware` (feature `timeout`): bounds the whole downstream
    chain per request, surfacing `FetchError::Timeout(d)` with the
    configured duration.
  - `BaseUrlMiddleware` (feature `base-url`): prefixes a base URL onto
    relative requests expressed with the reserved marker host
    `fetch-kit.relative` (`BaseUrlMiddleware::relative`); absolute URLs
    pass through untouched.
- **Request-scoped extensions** (breaking): `RequestBuilder::with_extension<T>`
  carries typed per-request data through the middleware chain's shared
  `extensions` map (`extensions.get::<T>()` in `Middleware::handle`) and
  survives retry attempts. (This is the reqwest-middleware extensions
  story — reqwest does not expose request extensions publicly.)
- `ClientBuilder::with_middleware`: register custom middleware
  **outermost** (wrapping the built-in stack) at build time.
- `Client::with_middleware`: append middleware **innermost** on a
  built client.
- `ClientWithMiddleware::with_middleware` / `middleware_count` /
  `execute_with_extensions` / `try_clone` on the middleware-level
  `RequestBuilder`.

### Changed

- **Breaking:** fetch-kit now ships its own middleware chain; the
  `reqwest-middleware` and `reqwest-retry` dependencies are gone.
  `Client::inner()` returns
  `&fetch_kit::middleware::ClientWithMiddleware`, and `Client::from_parts`
  takes that type. Retry backoff uses the `loop-retry` curve
  (exponential ×2, capped, ≤10% jitter) instead of `reqwest-retry`'s
  full jitter; the `retries` / `retry_bounds` knob semantics are
  unchanged.
- **Breaking:** the `retry` feature gates the built-in retry (and the
  `retries`/`retry_bounds` builder knobs); it is in the default feature
  set. JSON-touching APIs are gated behind the `json` feature, so
  `cargo check --no-default-features` now passes (it did not in 0.1.x).

### Kept

- The built-in resilience stack is unchanged in behavior: default
  3 retries / 500ms–30s bounds, optional circuit breaker via
  `with_breaker`, eager `base_url` resolution, typed JSON helpers, and
  the full wire-test matrix. Built-ins remain the defaults; the bundled
  middleware are the composition points. The default chain composes
  `[user middleware…] → retry → breaker → transport` — the breaker sees
  every retry attempt, and `CircuitOpen` is never retried (proven in
  `tests/middleware.rs` `retry_breaker`).

## [0.1.3] - 2026-09-12

### Added

- `tests/config_matrix.rs` (13 tests): behavior-observable wire coverage for
  the remaining builder/request knobs — `default_header`/`default_headers`
  (stamped on every request; per-request headers take precedence),
  `user_agent`, `reqwest_builder` (raw config applied verbatim),
  `retry_bounds` (pacing observably changes; jitter-aware assertion),
  `timeout` and `base_url` default-vs-configured contrasts, and the
  `RequestBuilder` `basic_auth`/`headers`/`form`/`json`/`query` shapes.
  Retry-count, timeout-override, breaker, and multipart knobs were already
  wire-proven in `tests/wire.rs`.

### Fixed

- **Dead knob:** `ClientBuilder::default_header`/`default_headers` stored
  into a field that `build()` never read — the headers silently never
  reached the wire. `try_build` now applies them to the underlying
  `reqwest` client (same release also carries 0.1.2's wire suite; this
  crate is not published to crates.io under this name).

## [0.1.2] - 2026-09-12

### Added

- Wire-level integration suite (`tests/wire.rs`, 16 tests) against real
  HTTP servers (wiremock): GET/POST/JSON round trips, base-URL resolution,
  header/auth/query propagation, PUT/DELETE passthrough, status-error
  mapping, primary/fallback recovery, client and per-request timeouts,
  retry-until-success / retry-exhaustion / no-retry-on-4xx, multipart
  upload shape, and the `circuit-breaker` feature over the wire (breaker
  opens after 3 consecutive 5xx and short-circuits with
  `FetchError::CircuitOpen`; stays closed through sub-threshold failures).

### Fixed

- Error fidelity: `FetchError::CircuitOpen` now surfaces as the
  `CircuitOpen` variant instead of being buried in
  `FetchError::Middleware(String)`. The middleware no longer erases the
  concrete type (`anyhow!` → `Into<anyhow::Error>`), and the
  `reqwest_middleware::Error → FetchError` conversion downcasts through
  both fetchkit's own middleware errors and `reqwest_retry::RetryError`
  wrapping. Callers matching on `FetchError::CircuitOpen` now work as the
  API always documented.

### CI

- New `integration` job running the wire suite with all features.

## [0.1.1] - 2026-09-11

### Fixed

- 22-gate quality audit pass: documentation completeness
  (README badges, REQUIREMENTS/THREAT-MODEL coverage) and
  feature-gated test hygiene.

## [0.1.0] - 2026-09-05

### Added
- Initial public release.
