# fetch-kit

[![Rust](https://img.shields.io/badge/rustc-1.85+-blue.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)

Resilient HTTP client for Rust — retry, circuit breaker, connection pooling, and typed JSON helpers built on reqwest.

> **Naming:** previously `fetchkit`; renamed to `fetch-kit` (estate
> convention) because `fetchkit` on crates.io belongs to an unrelated
> project (maintained by `everruns`). Published as **`fetch-kit`**.

## Features

- **Automatic retries** with exponential backoff via `reqwest-retry`
- **Typed JSON helpers** — `get_json` / `post_json` deserialize responses directly
- **Fallback fetching** — try a primary URL, fall back to another on failure
- **Circuit breaker** (opt-in) — stop hammering a dead service
- **Connection pooling** — inherits reqwest's connection pool
- **Pluggable middleware** — built on `reqwest-middleware`

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

## Comparison with raw reqwest

| | raw reqwest | fetch-kit |
|---|---|---|
| Retries | manual or separate middleware | built-in (configurable) |
| JSON helpers | `resp.json::<T>()` each time | `get_json` / `post_json` |
| Fallback | write it yourself | `fetch_with_fallback` |
| Timeout | per-client or per-request | builder default (30s) |

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
