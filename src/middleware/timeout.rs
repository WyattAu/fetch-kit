//! Timeout middleware — a per-request wall-clock bound on the entire
//! remainder of the middleware chain plus the transport.
//!
//! Unlike the builder-level client timeout (enforced by reqwest around
//! the raw HTTP call), this middleware bounds everything downstream of
//! it: inner middleware **and** the request itself. When the bound
//! elapses the request is cancelled and
//! [`crate::FetchError::Timeout`] is surfaced.
//!
//! Requires the `timeout` feature.

use std::time::Duration;

use async_trait::async_trait;
use http::Extensions;
use reqwest::{Request, Response};

use super::{Error, Middleware, Next, Result};
use crate::error::FetchError;

/// Per-request timeout middleware.
#[derive(Debug, Clone)]
pub struct TimeoutMiddleware {
    timeout: Duration,
}

impl TimeoutMiddleware {
    /// Create a timeout middleware bounding each request to `timeout`.
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

#[async_trait]
impl Middleware for TimeoutMiddleware {
    async fn handle(
        &self,
        req: Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> Result<Response> {
        match tokio::time::timeout(self.timeout, next.run(req, extensions)).await {
            Ok(result) => result,
            Err(_elapsed) => Err(Error::Middleware(
                // The concrete type is preserved through the error
                // downcast in `FetchError::from`, so callers match on
                // `FetchError::Timeout(self.timeout)` with the exact
                // configured duration.
                FetchError::Timeout(self.timeout).into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // test assertions unwrap by design
    use super::*;

    #[test]
    fn configured_duration_is_stored() {
        let mw = TimeoutMiddleware::new(Duration::from_millis(250));
        assert_eq!(mw.timeout, Duration::from_millis(250));
    }
}
