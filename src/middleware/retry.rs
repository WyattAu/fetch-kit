//! Retry middleware — estate loop-retry semantics (`loop-retry` crate)
//! applied per middleware attempt.
//!
//! Retryable outcomes are transport timeouts/connect errors and retryable
//! response statuses (408, 429, 5xx except 501). Middleware errors are
//! **never** retried: a downstream short-circuit such as
//! [`crate::FetchError::CircuitOpen`] surfaces immediately.
//!
//! When retries are exhausted, the final outcome is returned as-is — a
//! retryable-status response is returned unchanged (callers decide what to
//! do with the status), matching reqwest-retry's transient-retry behavior
//! that fetch-kit 0.1.x defaulted to.
//!
//! Backoff delegates to `loop_retry::RetryConfig` (exponential growth with
//! a multiplier of 2.0, capped at `max_delay`, optional 10% jitter), so
//! the timing semantics are the estate's single source of truth.

use std::fmt;

use async_trait::async_trait;
use http::Extensions;
use loop_retry::{IsRetryable, RetryConfig, RetryError, with_backoff};
use reqwest::{Request, Response, StatusCode};

use super::{Error, Middleware, Next, Result};

/// Retry middleware wrapping the remainder of the chain in the estate's
/// `loop-retry` backoff loop.
///
/// Defaults used by [`crate::ClientBuilder`]: 3 retries, 500ms initial
/// delay, 30s cap, jitter on.
#[derive(Debug, Clone)]
pub struct RetryMiddleware {
    config: RetryConfig,
}

impl RetryMiddleware {
    /// Create a retry middleware with the given loop-retry configuration.
    pub fn new(config: RetryConfig) -> Self {
        Self { config }
    }

    /// Create a retry middleware with `loop_retry::RetryConfig::default()`
    /// (3 retries, 100ms initial, 5s cap, jitter on).
    pub fn with_defaults() -> Self {
        Self::new(RetryConfig::default())
    }
}

/// Whether `status` is retried by [`RetryMiddleware`]:
/// 408 Request Timeout, 429 Too Many Requests, and 5xx except 501 Not
/// Implemented.
pub fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || (status.is_server_error() && status != StatusCode::NOT_IMPLEMENTED)
}

/// Whether `err` is retried by [`RetryMiddleware`]: transport-level
/// timeouts and connection failures. Middleware errors are permanent by
/// design — a short-circuit (e.g. an open circuit breaker) must not be
/// masked by retries.
pub fn is_retryable_error(err: &Error) -> bool {
    match err {
        Error::Reqwest(e) => e.is_timeout() || e.is_connect(),
        Error::Middleware(_) => false,
    }
}

/// The classification of one attempt, fed to `loop_retry`'s
/// `IsRetryable` to drive the backoff loop.
enum AttemptFailure {
    /// Transport timeout/connect error — retryable.
    Transient(Error),
    /// Response with a retryable status — retry until success or
    /// exhaustion; the final response is returned unchanged.
    RetryableStatus(Response),
    /// Anything else — surfaced immediately, never retried.
    Permanent(Error),
}

impl fmt::Display for AttemptFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transient(e) | Self::Permanent(e) => write!(f, "{e}"),
            Self::RetryableStatus(resp) => write!(f, "retryable status {}", resp.status()),
        }
    }
}

impl IsRetryable for AttemptFailure {
    fn is_retryable(&self) -> bool {
        match self {
            Self::Transient(_) | Self::RetryableStatus(_) => true,
            Self::Permanent(_) => false,
        }
    }
}

#[async_trait]
impl Middleware for RetryMiddleware {
    async fn handle(
        &self,
        req: Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> Result<Response> {
        // Own the request in a slot so each loop iteration can hand an
        // owned clone to the (required-'static) attempt future, and own
        // the shared per-request extensions so each attempt runs against
        // an independent snapshot.
        let mut slot = Some(req);
        let shared = std::mem::take(extensions);

        let outcome = with_backoff(&self.config, || {
            let attempt_req = slot.take();
            // Leave a clone in the slot for the next iteration (None for
            // streaming bodies, which cannot be retried).
            slot = attempt_req.as_ref().and_then(Request::try_clone);
            let snapshot = shared.clone();
            let next = next.clone();
            async move {
                let Some(req) = attempt_req else {
                    return Err(AttemptFailure::Permanent(Error::Middleware(
                        "request body is not cloneable; cannot retry".into(),
                    )));
                };
                let mut snapshot = snapshot;
                match next.run(req, &mut snapshot).await {
                    Ok(resp) if is_retryable_status(resp.status()) => {
                        Err(AttemptFailure::RetryableStatus(resp))
                    }
                    Ok(resp) => Ok(resp),
                    Err(e) if is_retryable_error(&e) => Err(AttemptFailure::Transient(e)),
                    Err(e) => Err(AttemptFailure::Permanent(e)),
                }
            }
        })
        .await;

        // Restore the (initial) shared extensions for outer middleware:
        // attempts ran on independent snapshots, so inner mutations made
        // during the retry loop are deliberately not propagated outward.
        *extensions = shared;

        match outcome {
            Ok(resp) => Ok(resp),
            Err(RetryError::FinalError(failure, _attempts)) => match failure {
                AttemptFailure::RetryableStatus(resp) => Ok(resp),
                AttemptFailure::Transient(e) | AttemptFailure::Permanent(e) => Err(e),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // test assertions unwrap by design
    use super::*;

    #[test]
    fn retryable_statuses_match_reqwest_retry_defaults() {
        assert!(is_retryable_status(StatusCode::REQUEST_TIMEOUT));
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_retryable_status(StatusCode::BAD_GATEWAY));
        assert!(is_retryable_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(!is_retryable_status(StatusCode::NOT_IMPLEMENTED));
        assert!(!is_retryable_status(StatusCode::NOT_FOUND));
        assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
        assert!(!is_retryable_status(StatusCode::OK));
    }

    #[test]
    fn middleware_errors_are_never_retryable() {
        let middleware_err = Error::Middleware("boom".into());
        assert!(!is_retryable_error(&middleware_err));
    }

    #[test]
    fn attempt_failure_classification_and_display() {
        let permanent = AttemptFailure::Permanent(Error::Middleware("boom".into()));
        assert!(permanent.to_string().contains("boom"));
        assert!(!permanent.is_retryable());
        assert!(AttemptFailure::Transient(Error::Middleware("x".into())).is_retryable());
    }
}
