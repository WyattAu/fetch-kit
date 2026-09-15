//! Middleware implementations for `fetch_kit`.

#[cfg(feature = "circuit-breaker")]
mod circuit_breaker;

#[cfg(feature = "circuit-breaker")]
pub use circuit_breaker::CircuitBreakerMiddleware;
