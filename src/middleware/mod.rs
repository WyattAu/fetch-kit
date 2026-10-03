//! Middleware: the core [`Middleware`] trait + [`ClientWithMiddleware`]
//! chain runner, plus the bundled feature-gated implementations.
//!
//! # Ordering semantics
//!
//! Middleware wrap the transport in an onion; the middleware registered
//! **first** is the **outermost** layer — it observes the request first
//! and the response last:
//!
//! ```text
//! ClientBuilder::new(client)
//!     .with(AuthMiddleware)       // 1st registered → outermost
//!     .with(RetryMiddleware::new(config))
//!     .build();
//!
//! request  →  Auth  →  Retry  →  transport
//! response ←  Auth  ←  Retry  ←  ╹
//! ```
//!
//! Inner middleware therefore see *every attempt* a retry-style outer
//! middleware issues, and outer middleware see only the final outcome.
//! The built-in default client composes `[retry, breaker]`: the breaker
//! sits inside the retry loop and records each HTTP attempt
//! individually.
//!
//! # Built-ins vs middleware (the composition story)
//!
//! The bundled middleware are the *composition points*; the
//! [`crate::ClientBuilder`] knobs are the *defaults*:
//!
//! | Knob (default) | Middleware equivalent (composable) |
//! |---|---|
//! | `retries` / `retry_bounds` | `RetryMiddleware` (feature `retry`, default-on) |
//! | `with_breaker` | `CircuitBreakerMiddleware` (feature `circuit-breaker`) |
//! | — | `ThrottleMiddleware` (feature `throttle`) |
//! | — | `TimeoutMiddleware` (feature `timeout`) |
//! | `base_url` (eager) | `BaseUrlMiddleware` (feature `base-url`) |
//!
//! [`crate::ClientBuilder::with_middleware`] registers custom middleware
//! **outermost** — wrapping the built-ins — so user middleware observe the
//! final outcome of the whole default stack.

#[cfg(feature = "base-url")]
mod base_url;
mod chain;
#[cfg(feature = "circuit-breaker")]
mod circuit_breaker;
#[cfg(feature = "retry")]
mod retry;
#[cfg(feature = "throttle")]
mod throttle;
#[cfg(feature = "timeout")]
mod timeout;

#[cfg(feature = "base-url")]
pub use base_url::{BaseUrlMiddleware, RELATIVE_MARKER_HOST};
pub use chain::{
    ClientBuilder, ClientWithMiddleware, Error, Middleware, Next, RequestBuilder, Result,
};
#[cfg(feature = "circuit-breaker")]
pub use circuit_breaker::CircuitBreakerMiddleware;
#[cfg(feature = "retry")]
pub use loop_retry::RetryConfig;
#[cfg(feature = "retry")]
pub use retry::{RetryMiddleware, is_retryable_error, is_retryable_status};
#[cfg(feature = "throttle")]
pub use throttle::ThrottleMiddleware;
#[cfg(feature = "timeout")]
pub use timeout::TimeoutMiddleware;
