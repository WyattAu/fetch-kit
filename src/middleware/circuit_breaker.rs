//! Circuit-breaker middleware wrapping the estate `breaker` crate.
//!
//! The breaker records every **HTTP attempt** it observes. Because the
//! built-in client composes `[retry, breaker]` (retry outermost), each
//! retry attempt passes the breaker individually — a flaky endpoint that
//! burns through retries trips the breaker that much sooner.

use http::Extensions;
use reqwest::{Request, Response};

use super::{Error, Middleware, Next, Result};
use crate::error::FetchError;

/// A middleware that wraps a [`breaker::CircuitBreaker`].
///
/// Before each request the circuit state is checked. If the circuit is
/// **Open** the request is short-circuited with [`FetchError::CircuitOpen`]
/// without touching the transport — and without being retried, since
/// [`super::RetryMiddleware`] treats middleware errors as permanent.
///
/// After a successful response (2xx / 3xx / 4xx other than 429) the breaker
/// records a success. On 5xx, 429, or network errors a failure is recorded.
pub struct CircuitBreakerMiddleware {
    breaker: breaker::CircuitBreaker,
}

impl CircuitBreakerMiddleware {
    /// Create a new middleware wrapping the given circuit breaker.
    pub fn new(breaker: breaker::CircuitBreaker) -> Self {
        Self { breaker }
    }
}

#[async_trait::async_trait]
impl Middleware for CircuitBreakerMiddleware {
    async fn handle(
        &self,
        req: Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> Result<Response> {
        use breaker::State;

        if self.breaker.state() == State::Open {
            // `FetchError` is kept as the concrete type inside the boxed
            // error, so `FetchError::from` can downcast it back and
            // surface `FetchError::CircuitOpen` to the caller.
            return Err(Error::Middleware(FetchError::CircuitOpen.into()));
        }

        match next.run(req, extensions).await {
            Ok(response) => {
                let status = response.status().as_u16();
                if status == 429 || status >= 500 {
                    self.breaker.record_failure();
                } else {
                    self.breaker.record_success();
                }
                Ok(response)
            }
            Err(err) => {
                self.breaker.record_failure();
                Err(err)
            }
        }
    }
}
