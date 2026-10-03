# fetch-kit

[![Rust](https://img.shields.io/badge/rustc-1.85+-blue.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)
[![docs.rs](https://docs.rs/fetch-kit/badge.svg)](https://docs.rs/fetch-kit)
[![crates.io](https://img.shields.io/crates/v/fetch-kit.svg)](https://crates.io/crates/fetch-kit)

Resilient HTTP client for Rust — retry, circuit breaker, connection pooling, and typed JSON helpers built on reqwest, with a native middleware stack that mirrors the `reqwest-middleware` shape.

> **Naming:** previously `fetchkit`; renamed to `fetch-kit` (estate
> convention) because `fetchkit` on crates.io belongs to an unrelated
> project (maintained by `everruns`). Published as **`fetch-kit`**.

## Features

- **Native middleware stack** — `Middleware` trait + `ClientWithMiddleware` chain with reqwest-middleware-compatible signatures, no extra dependency
- **Automatic retries** with exponential backoff + jitter (estate `loop-retry` semantics)
- **Typed JSON helpers** — `get_json` / `post_json` deserialize responses directly
- **Fallback fetching** — try a primary URL, fall back to another on failure
- **Circuit breaker** (opt-in) — stop hammering a dead service
- **Bundled middleware** — throttle (per-host token bucket), per-request timeout, base-URL prefixing
- **Connection pooling** — inherits reqwest's connection pool

## Quick Start

```rust
use fetch_kit::Client;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::builder()
        .base_url("https://api.example.com")
        .timeout(Duration::from_secs(10))
        .retries(5)
        .build();

    let user: serde_json::Value = client.get_json("/users/1").await?;
    println!("{user:#}");

    Ok(())
}
```

## Middleware

`fetch_kit::middleware::Middleware` is the composition point. The trait
shape mirrors `reqwest-middleware` 0.5, so migrating an existing
middleware is an import-path swap:

```rust
use async_trait::async_trait;
use fetch_kit::middleware::{Middleware, Next, Result};
use http::Extensions;
use reqwest::{Request, Response};

pub struct AuthMiddleware { token: String }

#[async_trait]
impl Middleware for AuthMiddleware {
    async fn handle(
        &self,
        mut req: Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> Result<Response> {
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {}", self.token).parse().unwrap(),
        );
        next.run(req, extensions).await
    }
}
```

Build a chain with `middleware::ClientBuilder` (wrapping any
`reqwest::Client`) or register middleware on the default stack with
`ClientBuilder::with_middleware` (user middleware run outermost — see
below):

```rust
use fetch_kit::ClientBuilder;

let client = ClientBuilder::new()
    .with_middleware(AuthMiddleware { token: "tok".into() })
    .build();
```

### Ordering semantics

Middleware wrap the transport in an onion. The middleware registered
**first** is the **outermost** layer — it observes the request first and
the response last:

```text
ClientBuilder::new(client)
    .with(AuthMiddleware)       // 1st registered → outermost
    .with(RetryMiddleware)      // 2nd registered
    .build();

request  →  Auth  →  Retry  →  transport
response ←  Auth  ←  Retry  ←  ╹
```

Inner middleware therefore see *every attempt* an outer retry-style
middleware issues; outer middleware see only the final outcome. The
`Next::run` continuation passes the (possibly rewritten) request inward;
returning without calling it short-circuits everything below.

### Bundled middleware

The core trait and chain are always available; each bundled
implementation sits behind its own feature:

| Middleware | Feature | Behavior |
|---|---|---|
| `RetryMiddleware` | `retry` (default-on) | Retries transport timeouts/connect errors and transient statuses (408, 429, 5xx except 501) with `loop-retry` exponential backoff + 10% jitter. Exhausted retryable statuses return the final response unchanged. |
| `CircuitBreakerMiddleware` | `circuit-breaker` | Short-circuits with `FetchError::CircuitOpen` when the estate breaker is open; records every attempt. |
| `ThrottleMiddleware` | `throttle` | Per-authority token bucket: sustained throughput never exceeds `rate` req/s, bursts up to `burst_capacity`. |
| `TimeoutMiddleware` | `timeout` | Bounds each request (and everything downstream of it) to a wall-clock duration; surfaces `FetchError::Timeout(d)`. |
| `BaseUrlMiddleware` | `base-url` | Prefixes a base URL onto relative requests (marker host `fetch-kit.relative`, built via `BaseUrlMiddleware::relative`). |

### Built-ins are defaults; middleware are composition points

The `ClientBuilder` knobs configure the built-in stack — `retries` /
`retry_bounds` shape the default retry middleware, `with_breaker` inserts
the breaker, `base_url` resolves paths eagerly. The default composition
(ordering: first = outermost) is:

```text
[user middleware…] → retry → breaker → transport
```

- **User middleware wrap the built-ins** (registered via
  `ClientBuilder::with_middleware`): they observe each logical request
  once, with final outcomes.
- **The breaker sits inside the retry loop**: every HTTP attempt records
  individually, so a flaky endpoint that burns retries trips the breaker
  that much sooner — and a short-circuit (`CircuitOpen`) is a middleware
  error, which retries never mask.

This is fetch-kit's differentiator over bare `reqwest-middleware`: the
resilience stack works out of the box, and the same pieces are available
as middleware when you want custom composition (e.g. throttle outside
retry, timeout inside it).

### Request-scoped extensions

Carry typed, per-request data through the middleware chain (and across
retry attempts) with `RequestBuilder::with_extension`:

```rust
#[derive(Clone)]
struct RequestId(u64);

let value: serde_json::Value = client
    .get("/users/1")
    .with_extension(RequestId(42))
    .json_response()
    .await?;
// inside Middleware::handle:  extensions.get::<RequestId>()
```

### Composing at the middleware level

```rust
use fetch_kit::middleware::{
    ClientBuilder, RetryConfig, RetryMiddleware, ThrottleMiddleware, TimeoutMiddleware,
};
use std::time::Duration;

let raw = ClientBuilder::new(reqwest::Client::new())
    .with(ThrottleMiddleware::new(50.0, 10))            // outermost
    .with(RetryMiddleware::new(RetryConfig::default()))
    .with(TimeoutMiddleware::new(Duration::from_secs(5)))
    .build();
```

## Circuit Breaker

Enable the `circuit-breaker` feature to wrap requests in a breaker that
automatically rejects calls after repeated failures.

```rust
use fetch_kit::Client;
use breaker::{CircuitBreaker, CircuitBreakerConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let breaker = CircuitBreaker::new(CircuitBreakerConfig::standard());

    let client = Client::builder()
        .base_url("https://api.example.com")
        .with_breaker(breaker)
        .build();

    // When the circuit opens, requests will return Err(FetchError::CircuitOpen).
    Ok(())
}
```

## Request Builder

```rust
use fetch_kit::Client;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::builder()
        .base_url("https://api.example.com")
        .default_header("Authorization", "Bearer token")
        .build();

    // Fluent request builder
    let value: serde_json::Value = client
        .get("/users/1")
        .timeout(Duration::from_secs(5))
        .send()
        .await?
        .json()
        .await?;

    // Or deserialize directly
    let user: serde_json::Value = client
        .post("/users")
        .json(&serde_json::json!({"name": "Alice"}))
        .json_response()
        .await?;

    Ok(())
}
```

## Migrating from 0.1.x

- **Your own middleware:** the trait shape is unchanged — swap
  `use reqwest_middleware::{Middleware, Next, Error, Result}` for
  `use fetch_kit::middleware::{Middleware, Next, Error, Result}`.
- **`Client::inner()`** now returns `&fetch_kit::middleware::ClientWithMiddleware`
  (fetch-kit's own chain, not reqwest-middleware's). `from_parts` takes
  that type.
- **Request-scoped data** moves from request fields to the
  `extensions` argument of `Middleware::handle` (populated via
  `RequestBuilder::with_extension`) — this matches how
  `reqwest-middleware` passes extensions, since reqwest does not expose
  request extensions publicly.
- **Backoff curve:** retries now use the estate `loop-retry` timing
  (exponential, ×2 growth, ≤10% jitter) instead of `reqwest-retry`'s
  full jitter; `retries`/`retry_bounds` semantics are unchanged.
- The `retry` feature (default-on) gates the built-in retry; disable
  default features to drop it.

## Comparison with raw reqwest

| | raw reqwest | fetch-kit |
|---|---|---|
| Retries | manual or separate middleware | built-in (configurable) or `RetryMiddleware` |
| Middleware | none (tower only) | `Middleware` trait, reqwest-middleware shape |
| Throttling | write it yourself | `ThrottleMiddleware` (per-host token bucket) |
| JSON helpers | `resp.json::<T>()` each time | `get_json` / `post_json` |
| Fallback | write it yourself | `fetch_with_fallback` |
| Timeout | per-client or per-request | builder default (30s) + `TimeoutMiddleware` |

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
