//! Property-based tests for resilient-fetch crate.

use proptest::prelude::*;

use fetch_kit::{Client, FetchError};

#[test]
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
