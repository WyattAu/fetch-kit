// Wire-level config-knob behavior matrix: every `ClientBuilder` /
// `RequestBuilder` knob must observably change what crosses the wire —
// default vs configured must differ. Request-shape knobs are proven with
// header/body matchers; timing knobs use generous bounds so the suite stays
// deterministic. Retry-count and breaker knobs are additionally proven in
// `tests/wire.rs`; this file focuses on the knobs wire.rs does not isolate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use fetch_kit::{ClientBuilder, FetchError};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
// default_header / default_headers — builder-level header stamping
// (dead-knob regression: these setters once stored a field that build()
// never read, so the headers silently never reached the wire)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn knob_default_header_stamps_every_request_on_the_wire() {
    let server = MockServer::start().await;
    // Both requests must carry the stamped header, on different methods.
    Mock::given(method("GET"))
        .and(path("/a"))
        .and(header("x-tenant", "acme"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"a"}"#))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/b"))
        .and(header("x-tenant", "acme"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"b"}"#))
        .expect(1)
        .mount(&server)
        .await;

    let client = ClientBuilder::new()
        .retries(0)
        .default_header("x-tenant", "acme")
        .build();

    let got: Simple = client
        .get_json(&format!("{}/a", server.uri()))
        .await
        .unwrap();
    assert_eq!(got.value, "a");
    let got: Simple = client
        .post_json(&format!("{}/b", server.uri()), &serde_json::json!({"x": 1}))
        .await
        .unwrap();
    assert_eq!(got.value, "b");
    server.verify().await;
}

#[tokio::test]
async fn knob_default_headers_absent_without_the_knob() {
    let server = MockServer::start().await;
    // Mounted FIRST: if a request arrived WITH the header, this mock would
    // serve it and its expect(0) verification would fail.
    Mock::given(header("x-tenant", "acme"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"plain"}"#))
        .mount(&server)
        .await;

    let plain = ClientBuilder::new().retries(0).build();
    let got: Simple = plain
        .get_json(&format!("{}/a", server.uri()))
        .await
        .unwrap();
    assert_eq!(got.value, "plain");
    server.verify().await;
}

#[tokio::test]
async fn knob_default_headers_map_applies_and_request_headers_take_precedence() {
    let server = MockServer::start().await;
    // Request-level override must win over the builder default.
    Mock::given(path("/override"))
        .and(header("x-role", "request-level"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"overridden"}"#))
        .expect(1)
        .mount(&server)
        .await;
    // Non-conflicting default must still be stamped.
    Mock::given(path("/keep"))
        .and(header("x-role", "request-level"))
        .and(header("x-keep", "default"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"kept"}"#))
        .expect(1)
        .mount(&server)
        .await;

    let mut headers = HeaderMap::new();
    headers.insert("x-role", HeaderValue::from_static("builder-default"));
    headers.insert("x-keep", HeaderValue::from_static("default"));

    let client = ClientBuilder::new()
        .retries(0)
        .default_headers(headers)
        .build();

    let got: Simple = client
        .get(format!("{}/override", server.uri()))
        .header("x-role", "request-level")
        .json_response()
        .await
        .unwrap();
    assert_eq!(got.value, "overridden");

    let got: Simple = client
        .get(format!("{}/keep", server.uri()))
        .header("x-role", "request-level")
        .json_response()
        .await
        .unwrap();
    assert_eq!(got.value, "kept");
    server.verify().await;
}

// ---------------------------------------------------------------------------
// user_agent and reqwest_builder — the raw builder path is used verbatim
// ---------------------------------------------------------------------------

#[tokio::test]
async fn knob_user_agent_reaches_the_wire() {
    let server = MockServer::start().await;
    Mock::given(header("user-agent", "matrix-ua/1.0"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"ua"}"#))
        .expect(1)
        .mount(&server)
        .await;

    let client = ClientBuilder::new()
        .retries(0)
        .user_agent("matrix-ua/1.0")
        .build();
    let got: Simple = client
        .get_json(&format!("{}/ua", server.uri()))
        .await
        .unwrap();
    assert_eq!(got.value, "ua");
    server.verify().await;
}

#[tokio::test]
async fn knob_reqwest_builder_config_applies_verbatim() {
    let server = MockServer::start().await;
    // The UA comes from the RAW reqwest builder (fetch_kit's own
    // user_agent setter was never called), proving the raw builder is used.
    Mock::given(header("user-agent", "raw-builder-ua/9"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"raw"}"#))
        .expect(1)
        .mount(&server)
        .await;

    let client = ClientBuilder::new()
        .retries(0)
        .reqwest_builder(reqwest::Client::builder().user_agent("raw-builder-ua/9"))
        .build();
    let got: Simple = client
        .get_json(&format!("{}/raw", server.uri()))
        .await
        .unwrap();
    assert_eq!(got.value, "raw");
    server.verify().await;
}

// ---------------------------------------------------------------------------
// retry_bounds — backoff bounds observably change retry pacing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn knob_retry_bounds_change_retry_pacing() {
    // Always-500 server; `retries(1)` → exactly 2 requests per run. The ONLY
    // observable difference between runs is client-side backoff pacing.
    //
    // reqwest-retry 0.9 / retry-policies 0.5 default to `Jitter::Full`: each
    // wait is uniform in [0, max_bound]. Individual waits are therefore
    // random, so instead of asserting on one wait we sum N runs: the sum of
    // N uniform draws concentrates hard around N * bound/2, making the floor
    // assertion deterministic-in-practice (failure probability < 1e-5).
    async fn timed_run(initial: Duration, max: Duration, runs: usize) -> Duration {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(500).set_body_string("down"))
            .expect(2 * runs as u64)
            .mount(&server)
            .await;
        let client = ClientBuilder::new()
            .retries(1)
            .retry_bounds(initial, max)
            .build();
        let start = Instant::now();
        for _ in 0..runs {
            let err = client
                .get_json::<Simple>(&format!("{}/x", server.uri()))
                .await
                .unwrap_err();
            assert!(matches!(err, FetchError::StatusCode { status: 500, .. }));
        }
        let elapsed = start.elapsed();
        server.verify().await;
        elapsed
    }

    // 8 runs x 1 retry x bound 250ms: waits are U[0, 250ms], sum mean 1000ms,
    // std ≈ 230ms → asserting ≥ 200ms cannot plausibly fail by chance, while
    // a dead knob (0 backoff) would finish in a few milliseconds.
    let slow = timed_run(Duration::from_millis(250), Duration::from_millis(250), 8).await;
    // Same shape at 1ms bounds: waits are U[0, 1ms]; even with loopback
    // overhead the total stays far below the slow run's floor.
    let fast = timed_run(Duration::from_millis(1), Duration::from_millis(1), 8).await;

    assert!(
        slow >= Duration::from_millis(200),
        "8 x U[0,250ms] backoff sums must concentrate near 1s, took {slow:?}"
    );
    assert!(
        fast < Duration::from_millis(300),
        "8 x U[0,1ms] backoff must stay near-instant, took {fast:?}"
    );
    assert!(
        fast * 4 < slow,
        "1ms-bound pacing ({fast:?}) must be clearly faster than 250ms-bound pacing ({slow:?})"
    );
}

// ---------------------------------------------------------------------------
// timeout and base_url — default vs configured contrast
// ---------------------------------------------------------------------------

#[tokio::test]
async fn knob_timeout_default_succeeds_where_configured_timeout_fails() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"value":"slow but inside default"}"#)
                .set_delay(Duration::from_millis(300)),
        )
        .expect(2) // both clients must reach the server
        .mount(&server)
        .await;

    let tight = ClientBuilder::new()
        .retries(0)
        .timeout(Duration::from_millis(50))
        .build();
    assert!(
        tight
            .get_json::<Simple>(&format!("{}/x", server.uri()))
            .await
            .is_err(),
        "50ms timeout must abort a ~300ms response"
    );

    let default = ClientBuilder::new().retries(0).build();
    let got: Simple = default
        .get_json(&format!("{}/x", server.uri()))
        .await
        .unwrap();
    assert_eq!(got.value, "slow but inside default");
    server.verify().await;
}

#[tokio::test]
async fn knob_base_url_absent_means_relative_paths_fail() {
    // Default (no base_url): a relative path cannot even be resolved.
    let plain = ClientBuilder::new().retries(0).build();
    assert!(
        plain.get_json::<Simple>("/relative").await.is_err(),
        "relative path without base_url must fail"
    );

    // Configured: the same relative path resolves against the base.
    let server = server_returning(200, r#"{"value":"based"}"#).await;
    let based = ClientBuilder::new()
        .retries(0)
        .base_url(server.uri())
        .build();
    let got: Simple = based.get_json("/relative").await.unwrap();
    assert_eq!(got.value, "based");
}

// ---------------------------------------------------------------------------
// RequestBuilder auth/body knobs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn knob_basic_auth_stamps_basic_scheme() {
    let server = MockServer::start().await;
    // "user:pass" → base64 "dXNlcjpwYXNz"
    Mock::given(header("authorization", "Basic dXNlcjpwYXNz"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"authed"}"#))
        .expect(1)
        .mount(&server)
        .await;

    let client = ClientBuilder::new().retries(0).build();
    let got: Simple = client
        .get(format!("{}/basic", server.uri()))
        .basic_auth("user", Some("pass"))
        .json_response()
        .await
        .unwrap();
    assert_eq!(got.value, "authed");
    server.verify().await;
}

#[tokio::test]
async fn knob_headers_map_stamps_all_entries() {
    let server = MockServer::start().await;
    Mock::given(header("x-first", "1"))
        .and(header("x-second", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"mapped"}"#))
        .expect(1)
        .mount(&server)
        .await;

    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("x-first"),
        HeaderValue::from_static("1"),
    );
    headers.insert(
        HeaderName::from_static("x-second"),
        HeaderValue::from_static("2"),
    );

    let client = ClientBuilder::new().retries(0).build();
    let got: Simple = client
        .get(format!("{}/mapped", server.uri()))
        .headers(headers)
        .json_response()
        .await
        .unwrap();
    assert_eq!(got.value, "mapped");
    server.verify().await;
}

#[tokio::test]
async fn knob_form_sends_urlencoded_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("name=wire"))
        .and(body_string_contains("count=7"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"formed"}"#))
        .expect(1)
        .mount(&server)
        .await;

    let client = ClientBuilder::new().retries(0).build();
    #[derive(serde::Serialize)]
    struct Form {
        name: &'static str,
        count: u32,
    }
    let got: Simple = client
        .post(format!("{}/form", server.uri()))
        .form(&Form {
            name: "wire",
            count: 7,
        })
        .json_response()
        .await
        .unwrap();
    assert_eq!(got.value, "formed");
    server.verify().await;
}

#[tokio::test]
async fn knob_request_json_sets_json_body_and_content_type() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("content-type", "application/json"))
        .and(body_string_contains("\"name\":\"matrix\""))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"jsoned"}"#))
        .expect(1)
        .mount(&server)
        .await;

    let client = ClientBuilder::new().retries(0).build();
    let got: Simple = client
        .post(format!("{}/json", server.uri()))
        .json(&serde_json::json!({"name": "matrix", "count": 1}))
        .json_response()
        .await
        .unwrap();
    assert_eq!(got.value, "jsoned");
    server.verify().await;
}

#[tokio::test]
async fn knob_query_params_reach_the_wire_from_builder() {
    let server = MockServer::start().await;
    Mock::given(query_param("page", "7"))
        .and(query_param("size", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"paged"}"#))
        .expect(1)
        .mount(&server)
        .await;

    let client = ClientBuilder::new().retries(0).build();
    let got: Simple = client
        .get(format!("{}/list", server.uri()))
        .query(&[("page", "7"), ("size", "50")])
        .json_response()
        .await
        .unwrap();
    assert_eq!(got.value, "paged");
    server.verify().await;
}
