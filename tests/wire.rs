// Wire tests exercise real HTTP round trips; unwrap/expect is the test
// signal here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Wire-level integration tests against a real HTTP server (wiremock).
//!
//! Every test starts a loopback server and drives the public [`fetch_kit`]
//! API over an actual TCP connection, so request shape, status handling,
//! retry behavior, timeouts, and the circuit breaker are validated at the
//! wire boundary rather than against mocks.
//!
//! TLS note: wiremock serves plain HTTP only, so the rustls/native-tls
//! feature paths are not exercised here — they are exercised by the
//! doctests against https endpoints and by reqwest's own test suite.

use std::time::Duration;

use fetch_kit::{Client, ClientBuilder, FetchError};
use serde::{Deserialize, Serialize};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Debug, Serialize, PartialEq)]
struct EchoRequest {
    name: String,
    count: u32,
}

#[derive(Debug, Deserialize, PartialEq)]
struct EchoResponse {
    name: String,
    count: u32,
    received_method: String,
}

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
// GET / POST / JSON round trips
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_json_roundtrip_over_the_wire() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "value": "hello wire"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let got: Simple = client
        .get_json(&format!("{}/items/42", server.uri()))
        .await
        .unwrap();
    assert_eq!(got.value, "hello wire");
}

#[tokio::test]
async fn post_json_sends_and_deserializes_body() {
    use wiremock::matchers::body_partial_json;

    let server = MockServer::start().await;
    // The matcher proves the JSON body crossed the wire intact; the
    // response proves client-side deserialization.
    Mock::given(method("POST"))
        .and(path("/echo"))
        .and(header("content-type", "application/json"))
        .and(body_partial_json(serde_json::json!({
            "name": "wire",
            "count": 7,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "name": "wire",
            "count": 7,
            "received_method": "POST",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let got: EchoResponse = client
        .post_json(
            &format!("{}/echo", server.uri()),
            &EchoRequest {
                name: "wire".into(),
                count: 7,
            },
        )
        .await
        .unwrap();
    assert_eq!(got.name, "wire");
    assert_eq!(got.count, 7);
    assert_eq!(got.received_method, "POST");
    server.verify().await;
}

#[tokio::test]
async fn base_url_is_prepended_to_relative_paths() {
    let server = server_returning(200, r#"{"value":"base"}"#).await;
    let client = ClientBuilder::new().base_url(server.uri()).build();
    let got: Simple = client.get_json("/res").await.unwrap();
    assert_eq!(got.value, "base");
    server.verify().await;
}

#[tokio::test]
async fn request_builder_headers_auth_and_query_reach_the_server() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/secure"))
        .and(header("authorization", "Bearer tok-123"))
        .and(header("x-trace", "abc"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"ok"}"#))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let got: Simple = client
        .get(format!("{}/secure", server.uri()))
        .header("x-trace", "abc")
        .bearer_auth("tok-123")
        .query(&[("page", "2")])
        .json_response()
        .await
        .unwrap();
    assert_eq!(got.value, "ok");
    server.verify().await;
}

#[tokio::test]
async fn put_and_delete_pass_through() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200).set_body_string("put"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::new();
    let resp = client
        .put(format!("{}/r", server.uri()))
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let resp = client
        .delete(format!("{}/r", server.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 204);
    server.verify().await;
}

// ---------------------------------------------------------------------------
// Status handling
// ---------------------------------------------------------------------------

#[tokio::test]
async fn non_2xx_maps_to_status_code_error_with_body() {
    let server = server_returning(404, "missing resource").await;
    let client = Client::new();
    let err = client
        .get_json::<Simple>(&format!("{}/nope", server.uri()))
        .await
        .unwrap_err();
    match err {
        FetchError::StatusCode { status, body } => {
            assert_eq!(status, 404);
            assert_eq!(body, "missing resource");
        }
        other => panic!("expected StatusCode error, got: {other:?}"),
    }
}

#[tokio::test]
async fn fetch_with_fallback_recovers_from_primary_failure() {
    let bad = server_returning(500, "boom").await;
    let good = server_returning(200, r#"{"value":"fallback"}"#).await;

    let client = Client::new();
    let got: Simple = client
        .fetch_with_fallback(
            &format!("{}/data", bad.uri()),
            &format!("{}/data", good.uri()),
        )
        .await
        .unwrap();
    assert_eq!(got.value, "fallback");
}

#[tokio::test]
async fn fetch_with_fallback_prefers_primary_when_healthy() {
    let good = server_returning(200, r#"{"value":"primary"}"#).await;

    let client = Client::new();
    let got: Simple = client
        .fetch_with_fallback(
            &format!("{}/data", good.uri()),
            "http://127.0.0.1:1/unreachable",
        )
        .await
        .unwrap();
    assert_eq!(got.value, "primary");
}

// ---------------------------------------------------------------------------
// Timeouts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn client_timeout_aborts_slow_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("slow")
                .set_delay(Duration::from_millis(2_000)),
        )
        .mount(&server)
        .await;

    let client = ClientBuilder::new()
        .timeout(Duration::from_millis(150))
        .retries(0)
        .build();
    let result = client
        .get_json::<Simple>(&format!("{}/slow", server.uri()))
        .await;
    assert!(
        result.is_err(),
        "slow response must trip the client timeout"
    );
}

#[tokio::test]
async fn per_request_timeout_override_applies() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"value":"slow"}"#)
                .set_delay(Duration::from_millis(2_000)),
        )
        .mount(&server)
        .await;

    let client = ClientBuilder::new()
        .timeout(Duration::from_secs(30))
        .retries(0)
        .build();
    let result = client
        .get(format!("{}/slow", server.uri()))
        .timeout(Duration::from_millis(150))
        .json_response::<Simple>()
        .await;
    assert!(result.is_err(), "per-request timeout override must apply");
}

// ---------------------------------------------------------------------------
// Retry behavior
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transient_500s_are_retried_until_success() {
    let server = MockServer::start().await;
    // First two requests fail with 500, every later request succeeds.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500).set_body_string("flaky"))
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"recovered"}"#))
        .expect(1)
        .mount(&server)
        .await;

    let client = ClientBuilder::new()
        .retries(3)
        .retry_bounds(Duration::from_millis(1), Duration::from_millis(5))
        .build();
    let got: Simple = client
        .get_json(&format!("{}/flaky", server.uri()))
        .await
        .unwrap();
    assert_eq!(got.value, "recovered");

    // Exactly two requests hit the failing mock before the healthy one.
    server.verify().await;
}

#[tokio::test]
async fn retries_exhausted_surface_the_last_error() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(500).set_body_string("always down"))
        .expect(3) // 1 initial + 2 retries
        .mount(&server)
        .await;

    let client = ClientBuilder::new()
        .retries(2)
        .retry_bounds(Duration::from_millis(1), Duration::from_millis(5))
        .build();
    let err = client
        .get_json::<Simple>(&format!("{}/down", server.uri()))
        .await
        .unwrap_err();
    assert!(matches!(err, FetchError::StatusCode { status: 500, .. }));
    server.verify().await;
}

#[tokio::test]
async fn four_oh_fours_are_not_retried() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(404).set_body_string("gone"))
        .expect(1) // 4xx is permanent: no retries
        .mount(&server)
        .await;

    let client = ClientBuilder::new()
        .retries(3)
        .retry_bounds(Duration::from_millis(1), Duration::from_millis(5))
        .build();
    let result = client
        .get_json::<Simple>(&format!("{}/gone", server.uri()))
        .await;
    assert!(result.is_err());
    server.verify().await;
}

// ---------------------------------------------------------------------------
// Circuit breaker (feature `circuit-breaker`)
// ---------------------------------------------------------------------------

#[cfg(feature = "circuit-breaker")]
mod breaker_wire {
    use super::*;
    use breaker::{CircuitBreaker, CircuitBreakerConfig};

    fn breaker() -> CircuitBreaker {
        CircuitBreaker::new(
            CircuitBreakerConfig::builder()
                .failure_rate_threshold(3) // consecutive failures before open
                .sliding_window_size(10)
                .wait_duration(Duration::from_secs(60))
                .build(),
        )
    }

    #[tokio::test]
    async fn breaker_opens_after_consecutive_failures_and_short_circuits() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(500).set_body_string("down"))
            .expect(3) // the 4th request must be short-circuited, never sent
            .mount(&server)
            .await;

        let client = ClientBuilder::new()
            .retries(0)
            .with_breaker(breaker())
            .build();

        // Three consecutive 5xx responses: the breaker trips on the third.
        for i in 0..3 {
            let err = client
                .get_json::<Simple>(&format!("{}/x", server.uri()))
                .await
                .unwrap_err();
            assert!(
                matches!(err, FetchError::StatusCode { status: 500, .. }),
                "req {i}: {err:?}"
            );
        }

        // Fourth request never reaches the server: the open circuit
        // short-circuits it with `FetchError::CircuitOpen`.
        let err = client
            .get_json::<Simple>(&format!("{}/x", server.uri()))
            .await
            .unwrap_err();
        assert!(
            matches!(err, FetchError::CircuitOpen),
            "expected CircuitOpen, got {err:?}"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn breaker_stays_closed_when_server_recovers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500).set_body_string("flaky"))
            .up_to_n_times(2)
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"value":"ok"}"#))
            .expect(3)
            .mount(&server)
            .await;

        let client = ClientBuilder::new()
            .retries(0)
            .with_breaker(breaker())
            .build();

        // Two failures: under the threshold of 3, so the circuit stays
        // closed.
        for i in 0..2 {
            let err = client
                .get_json::<Simple>(&format!("{}/x", server.uri()))
                .await
                .unwrap_err();
            assert!(
                matches!(err, FetchError::StatusCode { status: 500, .. }),
                "req {i}: {err:?}"
            );
        }
        // Recovery: the next three succeed and never see CircuitOpen.
        for i in 0..3 {
            client
                .get_json::<Simple>(&format!("{}/x", server.uri()))
                .await
                .unwrap_or_else(|e| panic!("req {i} must succeed: {e:?}"));
        }
        server.verify().await;
    }
}

// ---------------------------------------------------------------------------
// Multipart (feature `multipart`)
// ---------------------------------------------------------------------------

#[cfg(feature = "multipart")]
#[tokio::test]
async fn multipart_upload_carries_text_and_file_parts() {
    use fetch_kit::Client;
    use wiremock::ResponseTemplate as RT;
    use wiremock::matchers::body_string_contains;

    let server = MockServer::start().await;
    // (Content-type carries a boundary suffix, so instead of matching the
    // header exactly we assert the multipart payload shape by its parts.)
    Mock::given(method("POST"))
        .and(body_string_contains("wire-file-bytes"))
        .and(body_string_contains("field-value"))
        .respond_with(RT::new(201).set_body_string("stored"))
        .expect(1)
        .mount(&server)
        .await;

    // Multipart bodies stream, so the retry middleware (which clones
    // requests) cannot wrap them — build a retry-free client via
    // `from_parts` and exercise that constructor path too.
    let no_retry = reqwest_middleware::ClientBuilder::new(reqwest::Client::new()).build();
    let client = Client::from_parts(no_retry, None);

    let form = reqwest::multipart::Form::new()
        .text("label", "field-value")
        .part(
            "file",
            reqwest::multipart::Part::bytes(b"wire-file-bytes".to_vec())
                .file_name("payload.bin")
                .mime_str("application/octet-stream")
                .unwrap(),
        );

    let resp = client
        .post(format!("{}/upload", server.uri()))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    assert_eq!(resp.text().await.unwrap(), "stored");
    server.verify().await;
}
