# resilient-fetch

**DEPRECATED: this crate has been renamed to
[fetch-kit](https://crates.io/crates/fetch-kit), which is its direct successor
with the same API surface plus circuit-breaker middleware. New code should use
fetch-kit; existing code should switch at the next minor version.**


Resilient HTTP client for Rust — retry, circuit breaker, connection pooling, and typed JSON helpers built on reqwest.

## Features

- **Automatic retries** with exponential backoff via `reqwest-retry`
- **Typed JSON helpers** — `get_json` / `post_json` deserialize responses directly
- **Fallback fetching** — try a primary URL, fall back to another on failure
- **Circuit breaker** (opt-in) — stop hammering a dead service
- **Connection pooling** — inherits reqwest's connection pool
- **Pluggable middleware** — built on `reqwest-middleware`

## Quick Start

```rust
use fetchkit::Client;
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

## Comparison with raw reqwest

| | raw reqwest | fetchkit |
|---|---|---|
| Retries | manual or separate middleware | built-in (configurable) |
| JSON helpers | `resp.json::<T>()` each time | `get_json` / `post_json` |
| Fallback | write it yourself | `fetch_with_fallback` |
| Timeout | per-client or per-request | builder default (30s) |

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
