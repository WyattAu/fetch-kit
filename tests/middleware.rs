//! Middleware-stack integration tests: ordering semantics, the bundled
//! middleware (retry / throttle / timeout / base-url / breaker), and
//! request-scoped extensions — all driven over real HTTP (wiremock).
//!
//! The suite exercises the *public composition paths*: middleware-level
//! `ClientBuilder`, `Client::with_middleware`, `Client::inner`, and
//! `RequestBuilder::with_extension`.
#![cfg(feature = "retry")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // wire tests: unwrap is the signal

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fetch_kit::middleware::{
    ClientBuilder as ChainBuilder, ClientWithMiddleware, Error as MiddlewareError, Middleware,
    Next, Result as MiddlewareResult,
};
use fetch_kit::{Client, ClientBuilder, FetchError};
use http::Extensions;
use serde::Deserialize;
use wiremock::MockServer;
use wiremock::{Mock, ResponseTemplate};

#[derive(Debug, Deserialize)]
struct Simple {
    value: String,
}

async fn server_returning(status: u16, body: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(status).set_body_string(body))
        .mount(&server)
        .await;
    server
}

// ---------------------------------------------------------------------------
// Ordering: first-registered = outermost (the onion)
// ---------------------------------------------------------------------------

/// Records enter/exit transitions into a shared log.
struct Recorder {
    log: Arc<Mutex<Vec<&'static str>>>,
    enter: &'static str,
    exit: &'static str,
}

impl Recorder {
    fn pair(log: Arc<Mutex<Vec<&'static str>>>, name: &'static str) -> Self {
        let exit: &'static str = Box::leak(format!("{name}-exit").into_boxed_str());
        Self {
            log,
            enter: name,
            exit,
        }
    }
}

#[async_trait]
impl Middleware for Recorder {
    async fn handle(
        &self,
        req: reqwest::Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> MiddlewareResult<reqwest::Response> {
        self.log.lock().unwrap().push(self.enter);
        let result = next.run(req, extensions).await;
        self.log.lock().unwrap().push(self.exit);
        result
    }
}

#[tokio::test]
async fn first_registered_middleware_is_outermost_onion_order() {
    let server = server_returning(200, r#"{"value":"ok"}"#).await;
    let log = Arc::new(Mutex::new(Vec::new()));

    let client = ClientBuilder::new()
        .retries(0)
        .with_middleware(Recorder::pair(log.clone(), "outer"))
        .with_middleware(Recorder::pair(log.clone(), "inner"))
        .build();

    let got: Simple = client
        .get_json(&format!("{}/x", server.uri()))
        .await
        .unwrap();
    assert_eq!(got.value, "ok");

    // Onion: outer sees the request first and the response last.
    assert_eq!(
        *log.lock().unwrap(),
        vec!["outer", "inner", "inner-exit", "outer-exit"]
    );
}

#[tokio::test]
async fn chain_builder_registers_outermost_first_too() {
    let server = server_returning(200, r#"{"value":"chain"}"#).await;
    let log = Arc::new(Mutex::new(Vec::new()));

    let raw = ChainBuilder::new(reqwest::Client::new())
        .with(Recorder::pair(log.clone(), "outer"))
        .with(Recorder::pair(log.clone(), "inner"))
        .build();

    let resp = raw.get(format!("{}/y", server.uri())).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    assert_eq!(
        *log.lock().unwrap(),
        vec!["outer", "inner", "inner-exit", "outer-exit"]
    );
}

/// Counts requests passing through.
struct CounterArc(Arc<AtomicU32>);

#[async_trait]
impl Middleware for CounterArc {
    async fn handle(
        &self,
        req: reqwest::Request,
        ext: &mut Extensions,
        next: Next<'_>,
    ) -> MiddlewareResult<reqwest::Response> {
        self.0.fetch_add(1, Ordering::SeqCst);
        next.run(req, ext).await
    }
}

#[tokio::test]
async fn cloned_client_shares_the_middleware_chain() {
    let server = server_returning(200, "any").await;
    let counter = Arc::new(AtomicU32::new(0));

    let raw = ChainBuilder::new(reqwest::Client::new())
        .with(CounterArc(counter.clone()))
        .build();

    let a = raw.clone();
    a.get(format!("{}/a", server.uri())).send().await.unwrap();
    raw.get(format!("{}/b", server.uri())).send().await.unwrap();

    // Clones share the same chain (Arc'd), so one counter sees both.
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn with_middleware_after_build_appends_innermost() {
    let server = server_returning(200, "any").await;
    let log = Arc::new(Mutex::new(Vec::new()));

    let raw = ChainBuilder::new(reqwest::Client::new())
        .with(Recorder::pair(log.clone(), "outer"))
        .build();
    assert_eq!(raw.middleware_count(), 1);

    // Appending after build puts the middleware inside the existing chain.
    let raw = raw.with_middleware(Arc::new(Recorder::pair(log.clone(), "appended")));
    assert_eq!(raw.middleware_count(), 2);

    raw.get(format!("{}/z", server.uri())).send().await.unwrap();
    assert_eq!(
        *log.lock().unwrap(),
        vec!["outer", "appended", "appended-exit", "outer-exit"]
    );
}

#[tokio::test]
async fn client_with_middleware_composes_after_the_built_client() {
    let server = server_returning(200, r#"{"value":"composed"}"#).await;
    let counter = Arc::new(AtomicU32::new(0));

    let client = ClientBuilder::new().retries(0).build();
    let client = client.with_middleware(CounterArc(counter.clone()));

    let got: Simple = client
        .get_json(&format!("{}/c", server.uri()))
        .await
        .unwrap();
    assert_eq!(got.value, "composed");
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// Request-scoped extensions (typed map through the chain, retry-safe)
// ---------------------------------------------------------------------------

/// Request-scoped marker carried via `with_extension`.
#[derive(Clone, Debug, PartialEq)]
struct ReqId(u32);

/// Captures the ReqId visible in the extensions map per attempt.
struct ExtensionProbe {
    seen: Arc<Mutex<Vec<Option<u32>>>>,
}

#[async_trait]
impl Middleware for ExtensionProbe {
    async fn handle(
        &self,
        req: reqwest::Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> MiddlewareResult<reqwest::Response> {
        let id = extensions.get::<ReqId>().map(|r| r.0);
        self.seen.lock().unwrap().push(id);
        next.run(req, extensions).await
    }
}

#[tokio::test]
async fn with_extension_is_visible_to_middleware_and_survives_retries() {
    let server = MockServer::start().await;
    // One 500 then success: the retry loop runs a second attempt, and the
    // extension must be present in both.
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(500).set_body_string("flaky"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"with-ext"}"#))
        .expect(1)
        .mount(&server)
        .await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    // `Client::with_middleware` appends INNERMOST — inside the built-in
    // retry loop — so the probe observes every attempt.
    let client = ClientBuilder::new()
        .retries(2)
        .retry_bounds(Duration::from_millis(1), Duration::from_millis(2))
        .build()
        .with_middleware(ExtensionProbe { seen: seen.clone() });

    let got: Simple = client
        .get(format!("{}/ext", server.uri()))
        .with_extension(ReqId(7))
        .json_response()
        .await
        .unwrap();
    assert_eq!(got.value, "with-ext");

    // Visible on attempt 1 AND attempt 2 (the typed map is shared across
    // the retry loop).
    assert_eq!(*seen.lock().unwrap(), vec![Some(7), Some(7)]);
}

#[tokio::test]
async fn requests_without_extensions_observe_none() {
    let server = server_returning(200, "any").await;
    let seen = Arc::new(Mutex::new(Vec::new()));

    let raw = ChainBuilder::new(reqwest::Client::new())
        .with(ExtensionProbe { seen: seen.clone() })
        .build();
    raw.get(format!("{}/noext", server.uri()))
        .send()
        .await
        .unwrap();

    assert_eq!(*seen.lock().unwrap(), vec![None]);
}

// ---------------------------------------------------------------------------
// RetryMiddleware (feature `retry`)
// ---------------------------------------------------------------------------

/// Middleware that always fails with a custom error, and counts attempts.
struct AlwaysFailsMiddleware {
    attempts: Arc<AtomicU32>,
}

#[async_trait]
impl Middleware for AlwaysFailsMiddleware {
    async fn handle(
        &self,
        _req: reqwest::Request,
        _extensions: &mut Extensions,
        _next: Next<'_>,
    ) -> MiddlewareResult<reqwest::Response> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(MiddlewareError::Middleware("permanent fault".into()))
    }
}

#[cfg(feature = "retry")]
#[tokio::test]
async fn retry_middleware_retries_retryable_statuses_then_succeeds() {
    use fetch_kit::middleware::{RetryConfig, RetryMiddleware};

    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(500).set_body_string("flaky"))
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"recovered"}"#))
        .expect(1)
        .mount(&server)
        .await;

    let raw = ChainBuilder::new(reqwest::Client::new())
        .with(RetryMiddleware::new(RetryConfig {
            max_retries: 3,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
            jitter: false,
            ..RetryConfig::default()
        }))
        .build();

    let resp = raw.get(format!("{}/r", server.uri())).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.text().await.unwrap(), r#"{"value":"recovered"}"#);
    server.verify().await;
}

#[cfg(feature = "retry")]
#[tokio::test]
async fn retry_middleware_does_not_retry_middleware_errors() {
    use fetch_kit::middleware::{RetryConfig, RetryMiddleware};

    let attempts = Arc::new(AtomicU32::new(0));
    let raw = ChainBuilder::new(reqwest::Client::new())
        .with(RetryMiddleware::new(RetryConfig {
            max_retries: 5,
            ..RetryConfig::default()
        }))
        .with(AlwaysFailsMiddleware {
            attempts: attempts.clone(),
        })
        .build();

    let err = raw.get("http://127.0.0.1:1/x").send().await.unwrap_err();
    assert!(
        matches!(err, MiddlewareError::Middleware(ref e) if e.to_string().contains("permanent fault")),
        "unexpected error: {err:?}"
    );
    // Exactly one attempt: middleware errors are permanent by design.
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[cfg(feature = "retry")]
#[tokio::test]
async fn retry_middleware_returns_last_response_when_retries_exhaust() {
    use fetch_kit::middleware::{RetryConfig, RetryMiddleware};

    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(503).set_body_string("down"))
        .expect(3) // 1 initial + 2 retries
        .mount(&server)
        .await;

    let raw = ChainBuilder::new(reqwest::Client::new())
        .with(RetryMiddleware::new(RetryConfig {
            max_retries: 2,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
            jitter: false,
            ..RetryConfig::default()
        }))
        .build();

    // Exhausted retryable-status responses are returned unchanged; the
    // caller decides what to do with the status (fetch_kit's Client
    // layer turns this into FetchError::StatusCode — see wire.rs).
    let resp = raw.get(format!("{}/x", server.uri())).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 503);
    server.verify().await;
}

// ---------------------------------------------------------------------------
// Retry × circuit-breaker interaction (built-in stack, feature `circuit-breaker`)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "retry", feature = "circuit-breaker"))]
mod retry_breaker {
    use super::*;
    use breaker::{CircuitBreaker, CircuitBreakerConfig};

    fn breaker(threshold: u32) -> CircuitBreaker {
        CircuitBreaker::new(
            CircuitBreakerConfig::builder()
                .failure_rate_threshold(threshold)
                .sliding_window_size(10)
                .wait_duration(Duration::from_secs(60))
                .build(),
        )
    }

    /// The built-in composition is `[retry, breaker]`: the breaker sits
    /// *inside* the retry loop, so each HTTP attempt records a failure
    /// individually. Exhausting retries therefore trips the breaker, and
    /// the next logical request short-circuits at the breaker — the
    /// `CircuitOpen` error is middleware-level, so the retry loop does
    /// NOT retry it.
    #[tokio::test]
    async fn exhausted_retries_trip_the_breaker_which_short_circuits_without_retry() {
        let server = MockServer::start().await;
        // Exactly 4 wire hits ever: 2 logical requests x retries(1).
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(500).set_body_string("down"))
            .expect(4)
            .mount(&server)
            .await;

        let client = ClientBuilder::new()
            .retries(1) // 2 attempts per logical request
            .retry_bounds(Duration::from_millis(1), Duration::from_millis(2))
            .with_breaker(breaker(4)) // opens after 4 consecutive failures
            .build();

        // Logical request 1: attempts 1-2 → breaker failures 1-2.
        let err = client
            .get_json::<Simple>(&format!("{}/x", server.uri()))
            .await
            .unwrap_err();
        assert!(
            matches!(err, FetchError::StatusCode { status: 500, .. }),
            "{err:?}"
        );

        // Logical request 2: attempts 3-4 → breaker failures 3-4; the
        // breaker trips on the 4th.
        let err = client
            .get_json::<Simple>(&format!("{}/x", server.uri()))
            .await
            .unwrap_err();
        assert!(
            matches!(err, FetchError::StatusCode { status: 500, .. }),
            "{err:?}"
        );

        // Logical request 3: retry middleware runs (outermost), calls
        // inward, the OPEN breaker short-circuits with CircuitOpen — a
        // middleware error, which the retry loop never retries. The
        // server must see nothing (expect(4) above already pins this).
        for i in 0..3 {
            let err = client
                .get_json::<Simple>(&format!("{}/x", server.uri()))
                .await
                .unwrap_err();
            assert!(
                matches!(err, FetchError::CircuitOpen),
                "short-circuited req {i}: {err:?}"
            );
        }
        server.verify().await;
    }

    /// The breaker counts *attempts*, not logical requests: retries(2)
    /// against a dead endpoint burns the breaker 3x faster than retries(0).
    #[tokio::test]
    async fn breaker_sees_each_retry_attempt_individually() {
        let server = MockServer::start().await;
        // 3 wire hits: one logical request with retries(2).
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(500).set_body_string("down"))
            .expect(3)
            .mount(&server)
            .await;

        let client = ClientBuilder::new()
            .retries(2)
            .retry_bounds(Duration::from_millis(1), Duration::from_millis(2))
            .with_breaker(breaker(3)) // opens exactly on the 3rd attempt
            .build();

        // The logical request still surfaces its underlying 500 (the
        // breaker trips only after the 3rd failure is recorded).
        let err = client
            .get_json::<Simple>(&format!("{}/x", server.uri()))
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::StatusCode { status: 500, .. }));

        // And the next logical request is short-circuited.
        let err = client
            .get_json::<Simple>(&format!("{}/x", server.uri()))
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::CircuitOpen));
        server.verify().await;
    }
}

// ---------------------------------------------------------------------------
// TimeoutMiddleware (feature `timeout`)
// ---------------------------------------------------------------------------

#[cfg(feature = "timeout")]
#[tokio::test]
async fn timeout_middleware_bounds_the_downstream_chain() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("slow")
                .set_delay(Duration::from_millis(2_000)),
        )
        .mount(&server)
        .await;

    let raw = ChainBuilder::new(reqwest::Client::new())
        .with(fetch_kit::middleware::TimeoutMiddleware::new(
            Duration::from_millis(150),
        ))
        .build();

    let start = Instant::now();
    let err = raw
        .get(format!("{}/slow", server.uri()))
        .send()
        .await
        .unwrap_err();
    assert!(
        matches!(err, MiddlewareError::Middleware(ref e) if e.to_string().contains("timed out")),
        "unexpected error: {err:?}"
    );
    assert!(start.elapsed() < Duration::from_millis(1_500));
}

#[cfg(feature = "timeout")]
#[tokio::test]
async fn timeout_middleware_passes_fast_responses_through() {
    let server = server_returning(200, r#"{"value":"fast"}"#).await;

    let raw = ChainBuilder::new(reqwest::Client::new())
        .with(fetch_kit::middleware::TimeoutMiddleware::new(
            Duration::from_secs(5),
        ))
        .build();

    let resp = raw
        .get(format!("{}/fast", server.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
}

#[cfg(feature = "timeout")]
#[tokio::test]
async fn timeout_error_surfaces_as_fetch_error_timeout_with_configured_duration() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("slow")
                .set_delay(Duration::from_millis(2_000)),
        )
        .mount(&server)
        .await;

    let client = ClientBuilder::new()
        .retries(0)
        .with_middleware(fetch_kit::middleware::TimeoutMiddleware::new(
            Duration::from_millis(120),
        ))
        .build();

    let err = client
        .get_json::<Simple>(&format!("{}/slow", server.uri()))
        .await
        .unwrap_err();
    match err {
        FetchError::Timeout(d) => assert_eq!(d, Duration::from_millis(120)),
        other => panic!("expected FetchError::Timeout(120ms), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// ThrottleMiddleware (feature `throttle`)
// ---------------------------------------------------------------------------

#[cfg(feature = "throttle")]
#[tokio::test]
async fn throttle_middleware_rate_bounds_sequential_requests() {
    let server = server_returning(204, "").await;

    let raw = ChainBuilder::new(reqwest::Client::new())
        .with(fetch_kit::middleware::ThrottleMiddleware::new(50.0, 1))
        .build();

    // Burst 1 absorbs the first request; the next 3 wait ~20ms each.
    let start = Instant::now();
    for _ in 0..4 {
        let resp = raw.get(format!("{}/t", server.uri())).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 204);
    }
    let elapsed = start.elapsed();
    let expected_min = Duration::from_millis(60); // (4-1)/50s
    assert!(
        elapsed >= expected_min,
        "4 requests at 50/s burst 1 must take >= {expected_min:?}, took {elapsed:?}"
    );
}

#[cfg(feature = "throttle")]
#[tokio::test]
async fn throttle_middleware_buckets_are_per_host() {
    let a = server_returning(204, "").await;
    let b = server_returning(204, "").await;

    // A punishing rate: 4 interleaved requests would need >= 600ms if the
    // two hosts shared one bucket.
    let raw = ChainBuilder::new(reqwest::Client::new())
        .with(fetch_kit::middleware::ThrottleMiddleware::new(5.0, 1))
        .build();

    let start = Instant::now();
    for _ in 0..2 {
        raw.get(format!("{}/a", a.uri())).send().await.unwrap();
        raw.get(format!("{}/b", b.uri())).send().await.unwrap();
    }
    let elapsed = start.elapsed();

    // Distinct hosts are independent: two parallel buckets each pace at
    // 5/s → ~1 wait per host pair, far below a shared-bucket 600ms floor.
    assert!(
        elapsed < Duration::from_millis(400),
        "per-host buckets must not serialize across hosts, took {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// BaseUrlMiddleware (feature `base-url`)
// ---------------------------------------------------------------------------

#[cfg(feature = "base-url")]
#[tokio::test]
async fn base_url_middleware_prefixes_relative_paths_and_preserves_query() {
    use fetch_kit::middleware::{BaseUrlMiddleware, RELATIVE_MARKER_HOST};

    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/v1/users/42"))
        .and(wiremock::matchers::query_param("active", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"base"}"#))
        .expect(1)
        .mount(&server)
        .await;

    // The base carries a path prefix that must survive joining.
    let base = reqwest::Url::parse(&format!("{}/v1/", server.uri())).unwrap();
    let raw = ChainBuilder::new(reqwest::Client::new())
        .with(BaseUrlMiddleware::new(base))
        .build();

    let relative = BaseUrlMiddleware::relative("/users/42?active=true").unwrap();
    assert_eq!(relative.host_str(), Some(RELATIVE_MARKER_HOST));

    let resp = raw.get(relative.as_str().to_owned()).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.text().await.unwrap(), r#"{"value":"base"}"#);
    server.verify().await;
}

#[cfg(feature = "base-url")]
#[tokio::test]
async fn base_url_middleware_passes_absolute_urls_through() {
    use fetch_kit::middleware::BaseUrlMiddleware;

    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_string("absolute"))
        .expect(1)
        .mount(&server)
        .await;

    // Base pointing somewhere completely different: absolute URLs are
    // never rewritten.
    let elsewhere = reqwest::Url::parse("http://127.0.0.1:9/never").unwrap();
    let raw = ChainBuilder::new(reqwest::Client::new())
        .with(BaseUrlMiddleware::new(elsewhere))
        .build();

    let resp = raw
        .get(format!("{}/direct", server.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "absolute");
    server.verify().await;
}

#[cfg(feature = "base-url")]
#[test]
fn base_url_relative_helper_normalizes_and_preserves_query() {
    use fetch_kit::middleware::BaseUrlMiddleware;

    let url = BaseUrlMiddleware::relative("users/1?x=1").unwrap();
    assert_eq!(url.path(), "/users/1");
    assert_eq!(url.query(), Some("x=1"));
}

// ---------------------------------------------------------------------------
// ClientWithMiddleware constructor parity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn from_parts_with_chain_builder_and_no_base_url() {
    let server = server_returning(200, r#"{"value":"parts"}"#).await;
    let raw = ChainBuilder::new(reqwest::Client::new()).build();
    let client = Client::from_parts(raw, None);

    let got: Simple = client
        .get_json(&format!("{}/p", server.uri()))
        .await
        .unwrap();
    assert_eq!(got.value, "parts");
}

#[test]
fn client_inner_exposes_chain_and_clone_shares_it() {
    let client = ClientBuilder::new()
        .base_url("https://api.example.com")
        .build();
    let inner: &ClientWithMiddleware = client.inner();
    // Default stack: retry (feature on) — nothing else configured.
    let expected = if cfg!(feature = "retry") { 1 } else { 0 };
    assert_eq!(inner.middleware_count(), expected);

    // Clones share the same chain and configuration.
    let cloned = client.clone();
    assert_eq!(cloned.inner().middleware_count(), expected);
}
