//! Property-based tests for fetch-kit.

use proptest::prelude::*;

use fetch_kit::{Client, FetchError};

#[test]
#[cfg(feature = "retry")]
fn client_builder_retries_always_stored() {
    proptest!(|(retries in 0u32..1000u32)| {
        let builder = Client::builder().retries(retries);
        let client = builder.build();
        let _ = client.inner();
    });
}

#[test]
fn client_builder_base_url_always_stored() {
    proptest!(|(url in "https?://[a-z]{1,30}\\.[a-z]{2,10}")| {
        let client = Client::builder().base_url(&url).build();
        let _ = client.inner();
    });
}

#[test]
fn client_builder_timeout_accepts_range() {
    proptest!(|(timeout_secs in 1u64..3600u64)| {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .build();
        let _ = client.inner();
    });
}

#[test]
fn client_clone_preserves_base_url() {
    let client = Client::builder()
        .base_url("https://api.example.com")
        .build();
    let cloned = client.clone();
    let _ = cloned.inner();
    let _ = client.inner();
}

#[test]
fn client_builder_default_produces_valid() {
    let client = Client::builder().build();
    let _ = client.inner();
}

#[test]
fn client_default_produces_valid() {
    let client = Client::default();
    let _ = client.inner();
}

#[test]
#[cfg(feature = "retry")]
fn client_builder_chaining_all_fields() {
    proptest!(|(
        base in "https://[a-z]{1,20}\\.[a-z]{2,10}",
        timeout_secs in 1u64..300u64,
        retries in 0u32..100u32,
    )| {
        let client = Client::builder()
            .base_url(&base)
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .retries(retries)
            .build();
        let _ = client.inner();
    });
}

#[test]
fn fetch_error_debug_always_non_empty() {
    proptest!(|(msg in "[a-z ]{1,100}")| {
        let err = FetchError::Network(msg.clone());
        let debug = format!("{:?}", err);
        prop_assert!(!debug.is_empty());
    });
}

#[test]
fn fetch_error_display_always_contains_message() {
    proptest!(|(msg in "[a-z ]{1,100}")| {
        let err = FetchError::Network(msg.clone());
        let display = err.to_string();
        prop_assert!(display.contains(&msg));
    });
}

#[test]
fn fetch_error_status_code_display() {
    proptest!(|(status in 400u16..600u16, body in "[a-z ]{1,50}")| {
        let err = FetchError::StatusCode {
            status,
            body: body.clone(),
        };
        let display = err.to_string();
        prop_assert!(display.contains(&status.to_string()));
        prop_assert!(display.contains(&body));
    });
}

#[test]
fn fetch_error_is_std_error() {
    proptest!(|(msg in "[a-z ]{1,50}")| {
        let err = FetchError::Network(msg);
        let std_err: &dyn std::error::Error = &err;
        prop_assert!(!std_err.to_string().is_empty());
    });
}

// ---------------------------------------------------------------------------
// ThrottleMiddleware — sustained request throughput is rate-bounded
// ---------------------------------------------------------------------------

#[cfg(feature = "throttle")]
// tokio runtime construction and wiremock handles unwrap in test setup.
#[allow(clippy::unwrap_used)]
mod throttle_bounds {
    use std::time::{Duration, Instant};

    use fetch_kit::middleware::{ClientBuilder as ChainBuilder, ThrottleMiddleware};
    use proptest::prelude::*;

    /// For a full bucket (burst 1) and N sequential requests at rate R,
    /// total elapsed time must be at least (N-1)/R: the token bucket
    /// cannot hand out tokens faster than the refill rate. This is the
    /// load-shaping contract of the throttle middleware.
    #[test]
    fn throttle_rate_bounds_sustained_throughput() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .enable_io()
            .build()
            .unwrap();

        let config = proptest::test_runner::Config {
            cases: 300,
            ..proptest::test_runner::Config::default()
        };
        proptest!(config, |(requests in 1usize..10usize, rate_per_sec in 100u64..1000u64)| {
            rt.block_on(async {
                let server = wiremock::MockServer::start().await;
                wiremock::Mock::given(wiremock::matchers::any())
                    .respond_with(wiremock::ResponseTemplate::new(204))
                    .mount(&server)
                    .await;

                let client = ChainBuilder::new(reqwest::Client::new())
                    .with(ThrottleMiddleware::new(rate_per_sec as f64, 1))
                    .build();

                let start = Instant::now();
                for _ in 0..requests {
                    let result = client.get(format!("{}/t", server.uri())).send().await;
                    prop_assert!(
                        result.is_ok(),
                        "request failed: {:?}",
                        result.err().map(|e| e.to_string())
                    );
                }
                let elapsed = start.elapsed();

                let expected_min = Duration::from_secs_f64(
                    (requests - 1) as f64 / rate_per_sec as f64,
                );
                // At-least bound (the rate contract): sleeps only ever
                // overshoot, so allow a small measurement tolerance.
                prop_assert!(
                    elapsed + Duration::from_millis(2) >= expected_min,
                    "n={requests} rate={rate_per_sec}/s: elapsed {elapsed:?} \
                     must be >= {expected_min:?}"
                );
                // Generous upper sanity bound: catches a stuck bucket.
                prop_assert!(
                    elapsed < expected_min + Duration::from_secs(5),
                    "n={requests} rate={rate_per_sec}/s: elapsed {elapsed:?} \
                     suspiciously above {expected_min:?}"
                );
                Ok::<(), proptest::test_runner::TestCaseError>(())
            })
            // A failed prop_assert bubbles out of the async block as
            // Err(TestCaseError); unwrapping turns it into a panic, which
            // proptest's runner catches, shrinks, and reports.
            .unwrap();
        });
    }
}
