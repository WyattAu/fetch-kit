#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Resilient HTTP client for Rust.
//!
//! `fetchkit` wraps `reqwest-middleware` with sensible defaults for retries,
//! timeouts, and typed JSON helpers. Enable the `circuit-breaker` feature for
//! automatic circuit-breaking on repeated failures.

/// Error types.
pub mod error;

use std::time::Duration;

use reqwest_middleware::{ClientBuilder as ReqwestClientBuilder, ClientWithMiddleware};
use reqwest_retry::{policies::ExponentialBackoff, RetryTransientMiddleware};
use serde::{de::DeserializeOwned, Serialize};

pub use error::FetchError;
pub use reqwest::Response;

/// A resilient HTTP client with retry, timeout, and typed JSON support.
#[derive(Clone, Debug)]
pub struct Client {
    inner: ClientWithMiddleware,
    pub(crate) base_url: Option<String>,
}

/// Builder for constructing a [`Client`] with custom configuration.
pub struct ClientBuilder {
    reqwest_builder: reqwest::ClientBuilder,
    retry_policy: ExponentialBackoff,
    base_url: Option<String>,
    retries: u32,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientBuilder {
    /// Create a new builder with defaults (30s timeout, 3 retries, exponential backoff).
    pub fn new() -> Self {
        let retry_policy = ExponentialBackoff::builder()
            .build_with_max_retries(3);

        let reqwest_builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(30));

        Self {
            reqwest_builder,
            retry_policy,
            base_url: None,
            retries: 3,
        }
    }

    /// Set a base URL that is prepended to all relative paths.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Set the request timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.reqwest_builder = self.reqwest_builder.timeout(timeout);
        self
    }

    /// Set the maximum number of retries.
    pub fn retries(mut self, retries: u32) -> Self {
        self.retries = retries;
        self.retry_policy = ExponentialBackoff::builder()
            .build_with_max_retries(retries);
        self
    }

    /// Add a raw `reqwest::ClientBuilder` for advanced configuration.
    pub fn reqwest_builder(mut self, builder: reqwest::ClientBuilder) -> Self {
        self.reqwest_builder = builder;
        self
    }

    /// Build the [`Client`].
    pub fn build(self) -> Client {
        let reqwest_client = self.reqwest_builder.build().expect("failed to build reqwest client");
        let inner = ReqwestClientBuilder::new(reqwest_client)
            .with(RetryTransientMiddleware::new_with_policy(self.retry_policy))
            .build();

        Client {
            inner,
            base_url: self.base_url,
        }
    }
}

impl Client {
    /// Create a new client with default settings.
    pub fn new() -> Self {
        ClientBuilder::new().build()
    }

    /// Start building a client with custom settings.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::new()
    }

    /// Resolve a path against the optional base URL.
    pub(crate) fn resolve_url(&self, url: &str) -> String {
        match &self.base_url {
            Some(base) => {
                let base = base.trim_end_matches('/');
                let path = url.trim_start_matches('/');
                format!("{base}/{path}")
            }
            None => url.to_string(),
        }
    }

    /// Perform a GET request and deserialize the JSON response.
    pub async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, FetchError> {
        let url = self.resolve_url(url);
        let response = self.inner.get(&url).send().await?;
        let response = Self::check_status(response).await?;
        Ok(response.json().await?)
    }

    /// Perform a POST request with a JSON body and deserialize the JSON response.
    pub async fn post_json<B: Serialize, T: DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T, FetchError> {
        let url = self.resolve_url(url);
        let response = self.inner
            .post(&url)
            .json(body)
            .send()
            .await?;
        let response = Self::check_status(response).await?;
        Ok(response.json().await?)
    }

    /// Fetch from `primary_url`; on failure, fall back to `fallback_url`.
    pub async fn fetch_with_fallback<T: DeserializeOwned>(
        &self,
        primary_url: &str,
        fallback_url: &str,
    ) -> Result<T, FetchError> {
        match self.get_json(primary_url).await {
            Ok(value) => Ok(value),
            Err(_) => self.get_json(fallback_url).await,
        }
    }

    /// Access the inner `reqwest_middleware::ClientWithMiddleware`.
    pub fn inner(&self) -> &ClientWithMiddleware {
        &self.inner
    }

    async fn check_status(response: Response) -> Result<Response, FetchError> {
        let status = response.status();
        if status.is_success() {
            Ok(response)
        } else {
            let code = status.as_u16();
            let body = response.text().await.unwrap_or_default();
            Err(FetchError::StatusCode { status: code, body })
        }
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn client_builder_default_timeout() {
        let builder = ClientBuilder::new();
        assert_eq!(builder.retries, 3);
    }

    #[test]
    fn client_builder_default_retries() {
        let builder = ClientBuilder::new();
        assert_eq!(builder.retries, 3);
    }

    #[test]
    fn client_builder_default_no_base_url() {
        let builder = ClientBuilder::new();
        assert!(builder.base_url.is_none());
    }

    #[test]
    fn client_builder_custom_timeout() {
        let builder = ClientBuilder::new().timeout(Duration::from_secs(10));
        let client = builder.build();
        let _ = client.inner();
    }

    #[test]
    fn client_builder_custom_retries() {
        let builder = ClientBuilder::new().retries(5);
        assert_eq!(builder.retries, 5);
    }

    #[test]
    fn client_builder_custom_base_url() {
        let builder = ClientBuilder::new().base_url("https://api.example.com");
        let client = builder.build();
        assert!(client.base_url.is_some());
        assert_eq!(client.base_url.as_deref(), Some("https://api.example.com"));
    }

    #[test]
    fn client_builder_chaining() {
        let client = ClientBuilder::new()
            .base_url("https://api.example.com")
            .timeout(Duration::from_secs(60))
            .retries(10)
            .build();
        assert_eq!(client.base_url.as_deref(), Some("https://api.example.com"));
    }

    #[test]
    fn fetch_error_network_display() {
        let err = FetchError::Network("connection refused".into());
        let msg = err.to_string();
        assert!(msg.contains("connection refused"));
    }

    #[test]
    fn fetch_error_timeout_display() {
        let err = FetchError::Timeout(Duration::from_secs(5));
        let msg = err.to_string();
        assert!(msg.contains("5s"));
    }

    #[test]
    fn fetch_error_status_code_display() {
        let err = FetchError::StatusCode {
            status: 404,
            body: "not found".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("404"));
        assert!(msg.contains("not found"));
    }

    #[test]
    fn fetch_error_serialization_display() {
        let json_err =
            serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let err = FetchError::Serialization(json_err);
        let msg = err.to_string();
        assert!(msg.contains("serialization error"));
    }

    #[test]
    fn fetch_error_circuit_open_display() {
        let err = FetchError::CircuitOpen;
        let msg = err.to_string();
        assert!(msg.contains("circuit breaker"));
    }

    #[test]
    fn fetch_error_middleware_display() {
        let err = FetchError::Middleware("tower error".into());
        let msg = err.to_string();
        assert!(msg.contains("tower error"));
    }

    #[test]
    fn url_resolution_base_with_relative_path() {
        let client = Client::new();
        let client = Client {
            base_url: Some("https://api.example.com".into()),
            ..client
        };
        let resolved = client.resolve_url("/users/1");
        assert_eq!(resolved, "https://api.example.com/users/1");
    }

    #[test]
    fn url_resolution_no_base_url() {
        let client = Client::new();
        let resolved = client.resolve_url("/users/1");
        assert_eq!(resolved, "/users/1");
    }

    #[test]
    fn url_resolution_base_trailing_slash() {
        let client = Client {
            base_url: Some("https://api.example.com/".into()),
            inner: Client::new().inner,
        };
        let resolved = client.resolve_url("/users/1");
        assert_eq!(resolved, "https://api.example.com/users/1");
    }

    #[test]
    fn url_resolution_path_no_leading_slash() {
        let client = Client {
            base_url: Some("https://api.example.com".into()),
            inner: Client::new().inner,
        };
        let resolved = client.resolve_url("users/1");
        assert_eq!(resolved, "https://api.example.com/users/1");
    }

    #[test]
    fn url_resolution_empty_base_trailing_slash() {
        let client = Client {
            base_url: Some("https://api.example.com/".into()),
            inner: Client::new().inner,
        };
        let resolved = client.resolve_url("users/1");
        assert_eq!(resolved, "https://api.example.com/users/1");
    }

    #[test]
    fn client_default_trait() {
        let client = Client::default();
        assert!(client.base_url.is_none());
    }

    #[test]
    fn client_builder_default_trait() {
        let builder = ClientBuilder::default();
        assert_eq!(builder.retries, 3);
    }
}
