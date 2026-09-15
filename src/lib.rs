#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Resilient HTTP client for Rust.
//!
//! `fetch_kit` wraps `reqwest-middleware` with sensible defaults for retries,
//! timeouts, and typed JSON helpers. Enable the `circuit-breaker` feature for
//! automatic circuit-breaking on repeated failures.

/// Error types.
pub mod error;
/// Middleware implementations.
pub mod middleware;

use std::time::Duration;

use reqwest_middleware::{ClientBuilder as ReqwestClientBuilder, ClientWithMiddleware};
use reqwest_retry::{RetryTransientMiddleware, policies::ExponentialBackoff};
use serde::{Serialize, de::DeserializeOwned};

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
    initial_backoff: Duration,
    max_backoff: Duration,
    default_headers: reqwest::header::HeaderMap,
    #[cfg(feature = "circuit-breaker")]
    breaker: Option<breaker::CircuitBreaker>,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientBuilder {
    /// Create a new builder with defaults (30s timeout, 3 retries, exponential backoff).
    pub fn new() -> Self {
        let retry_policy = ExponentialBackoff::builder().build_with_max_retries(3);

        let reqwest_builder = reqwest::Client::builder().timeout(Duration::from_secs(30));

        Self {
            reqwest_builder,
            retry_policy,
            base_url: None,
            retries: 3,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            default_headers: reqwest::header::HeaderMap::new(),
            #[cfg(feature = "circuit-breaker")]
            breaker: None,
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
        self.retry_policy = ExponentialBackoff::builder().build_with_max_retries(retries);
        self
    }

    /// Set the initial and maximum retry backoff durations.
    pub fn retry_bounds(mut self, initial: Duration, max: Duration) -> Self {
        self.initial_backoff = initial;
        self.max_backoff = max;
        self.retry_policy = ExponentialBackoff::builder()
            .retry_bounds(initial, max)
            .build_with_max_retries(self.retries);
        self
    }

    /// Add a default header applied to every request (infallible for
    /// compile-time string literals).
    ///
    /// # Panics
    /// Panics if `key` or `value` is not a valid HTTP header string —
    /// this is a caller programming error, not a runtime condition.
    /// Use [`default_headers`](Self::default_headers) with a `HeaderMap`
    /// for a fallible variant.
    #[allow(clippy::expect_used)]
    pub fn default_header(mut self, key: &'static str, value: &'static str) -> Self {
        let name = http::header::HeaderName::from_static(key);
        let val = http::HeaderValue::from_str(value).expect("invalid header value");
        self.default_headers.insert(name, val);
        self
    }

    /// Set multiple default headers applied to every request.
    pub fn default_headers(mut self, headers: reqwest::header::HeaderMap) -> Self {
        for (key, value) in headers {
            if let Some(key) = key {
                self.default_headers.insert(key, value);
            }
        }
        self
    }

    /// Set the `User-Agent` header.
    pub fn user_agent(mut self, agent: &'static str) -> Self {
        self.reqwest_builder = self.reqwest_builder.user_agent(agent);
        self
    }

    /// Add a raw `reqwest::ClientBuilder` for advanced configuration.
    pub fn reqwest_builder(mut self, builder: reqwest::ClientBuilder) -> Self {
        self.reqwest_builder = builder;
        self
    }

    /// Attach a circuit breaker to the client (requires the `circuit-breaker` feature).
    #[cfg(feature = "circuit-breaker")]
    pub fn with_breaker(mut self, breaker: breaker::CircuitBreaker) -> Self {
        self.breaker = Some(breaker);
        self
    }

    /// Build the [`Client`].
    ///
    /// # Panics
    ///
    /// Panics if the underlying `reqwest::Client` cannot be built. This should
    /// only happen due to process-startup defects (e.g. invalid TLS config),
    /// not from user input at runtime.
    #[allow(clippy::expect_used)]
    pub fn build(self) -> Client {
        self.try_build()
            .expect("failed to build reqwest client — this is a process-startup defect")
    }

    /// Build the [`Client`], returning a [`FetchError`] on failure.
    pub fn try_build(self) -> Result<Client, FetchError> {
        let reqwest_client = self
            .reqwest_builder
            // Builder-level default headers must actually reach the wire:
            // they apply to every request, with per-request headers taking
            // precedence on conflict.
            .default_headers(self.default_headers)
            .build()
            .map_err(|e| FetchError::BuildError(e.to_string()))?;

        let builder = ReqwestClientBuilder::new(reqwest_client)
            .with(RetryTransientMiddleware::new_with_policy(self.retry_policy));

        #[cfg(feature = "circuit-breaker")]
        let builder = match self.breaker {
            Some(breaker) => builder.with(middleware::CircuitBreakerMiddleware::new(breaker)),
            None => builder,
        };

        let inner = builder.build();

        Ok(Client {
            inner,
            base_url: self.base_url,
        })
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

    /// Construct a [`Client`] from a pre-built [`ClientWithMiddleware`] and
    /// an optional base URL.
    ///
    /// This is useful when you need to attach custom middleware (e.g. a
    /// circuit breaker from an external registry) that fetch_kit's builder
    /// does not natively support.
    pub fn from_parts(inner: ClientWithMiddleware, base_url: Option<String>) -> Self {
        Self { inner, base_url }
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

    /// Create a [`RequestBuilder`] for a GET request.
    pub fn get(&self, url: impl Into<String>) -> RequestBuilder<'_> {
        let url = self.resolve_url(&url.into());
        RequestBuilder {
            client: self,
            builder: self.inner.get(&url),
        }
    }

    /// Create a [`RequestBuilder`] for a POST request.
    pub fn post(&self, url: impl Into<String>) -> RequestBuilder<'_> {
        let url = self.resolve_url(&url.into());
        RequestBuilder {
            client: self,
            builder: self.inner.post(&url),
        }
    }

    /// Create a [`RequestBuilder`] for a PUT request.
    pub fn put(&self, url: impl Into<String>) -> RequestBuilder<'_> {
        let url = self.resolve_url(&url.into());
        RequestBuilder {
            client: self,
            builder: self.inner.put(&url),
        }
    }

    /// Create a [`RequestBuilder`] for a DELETE request.
    pub fn delete(&self, url: impl Into<String>) -> RequestBuilder<'_> {
        let url = self.resolve_url(&url.into());
        RequestBuilder {
            client: self,
            builder: self.inner.delete(&url),
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
        let response = self.inner.post(&url).json(body).send().await?;
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

/// A wrapper around `reqwest_middleware::RequestBuilder` that provides a
/// ergonomic API and resolves the URL against the client's base URL.
pub struct RequestBuilder<'a> {
    #[allow(dead_code)]
    client: &'a Client,
    builder: reqwest_middleware::RequestBuilder,
}

impl<'a> RequestBuilder<'a> {
    /// Add a single header.
    pub fn header<K, V>(mut self, key: K, value: V) -> Self
    where
        http::header::HeaderName: TryFrom<K>,
        <http::header::HeaderName as TryFrom<K>>::Error: Into<http::Error>,
        http::HeaderValue: TryFrom<V>,
        <http::HeaderValue as TryFrom<V>>::Error: Into<http::Error>,
    {
        self.builder = self.builder.header(key, value);
        self
    }

    /// Set all headers from a `HeaderMap`.
    pub fn headers(mut self, headers: reqwest::header::HeaderMap) -> Self {
        self.builder = self.builder.headers(headers);
        self
    }

    /// Append query parameters.
    pub fn query<T: Serialize + ?Sized>(mut self, params: &T) -> Self {
        self.builder = self.builder.query(params);
        self
    }

    /// Override the request timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.builder = self.builder.timeout(timeout);
        self
    }

    /// Set a JSON body.
    pub fn json<T: Serialize + ?Sized>(mut self, body: &T) -> Self {
        self.builder = self.builder.json(body);
        self
    }

    /// Set a `Bearer` authorization token.
    pub fn bearer_auth(mut self, token: impl std::fmt::Display) -> Self {
        self.builder = self.builder.bearer_auth(token);
        self
    }

    /// Set HTTP basic auth credentials.
    pub fn basic_auth(
        mut self,
        username: impl std::fmt::Display,
        password: Option<impl std::fmt::Display>,
    ) -> Self {
        self.builder = self.builder.basic_auth(username, password);
        self
    }

    /// Set the request body directly.
    pub fn body(mut self, body: impl Into<reqwest::Body>) -> Self {
        self.builder = self.builder.body(body);
        self
    }

    /// Set form-encoded body from a serializable value.
    pub fn form<T: Serialize + ?Sized>(mut self, form: &T) -> Self {
        self.builder = self.builder.form(form);
        self
    }

    /// Set a multipart form body (requires the `multipart` feature).
    #[cfg(feature = "multipart")]
    pub fn multipart(mut self, form: reqwest::multipart::Form) -> Self {
        self.builder = self.builder.multipart(form);
        self
    }

    /// Send the request.
    pub async fn send(self) -> Result<Response, FetchError> {
        let response = self.builder.send().await?;
        Client::check_status(response).await
    }

    /// Send the request and deserialize the JSON response.
    pub async fn json_response<T: DeserializeOwned>(self) -> Result<T, FetchError> {
        let response = self.send().await?;
        Ok(response.json().await?)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // test assertions unwrap by design
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
        let json_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
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

    // ---- Additional ClientBuilder tests ----

    #[test]
    fn client_builder_new_creates_valid_builder() {
        let builder = ClientBuilder::new();
        assert_eq!(builder.retries, 3);
        assert!(builder.base_url.is_none());
        let client = builder.build();
        assert!(client.base_url.is_none());
    }

    #[test]
    fn client_builder_base_url_strips_trailing_slash_on_build() {
        let client = ClientBuilder::new()
            .base_url("https://api.example.com/")
            .build();
        let resolved = client.resolve_url("/users/1");
        assert_eq!(resolved, "https://api.example.com/users/1");
    }

    #[test]
    fn client_builder_timeout_zero() {
        let client = ClientBuilder::new().timeout(Duration::from_secs(0)).build();
        let _ = client.inner();
    }

    #[test]
    fn client_builder_timeout_large() {
        let client = ClientBuilder::new()
            .timeout(Duration::from_secs(3600))
            .build();
        let _ = client.inner();
    }

    #[test]
    fn client_builder_retries_zero() {
        let builder = ClientBuilder::new().retries(0);
        assert_eq!(builder.retries, 0);
    }

    #[test]
    fn client_builder_retries_large() {
        let builder = ClientBuilder::new().retries(1000);
        assert_eq!(builder.retries, 1000);
    }

    #[test]
    fn client_builder_overwrite_retries() {
        let builder = ClientBuilder::new().retries(5).retries(10);
        assert_eq!(builder.retries, 10);
    }

    #[test]
    fn client_builder_overwrite_timeout() {
        let client = ClientBuilder::new()
            .timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build();
        let _ = client.inner();
    }

    #[test]
    fn client_builder_full_chain() {
        let client = ClientBuilder::new()
            .base_url("https://api.example.com")
            .timeout(Duration::from_secs(15))
            .retries(7)
            .build();
        assert_eq!(client.base_url.as_deref(), Some("https://api.example.com"));
    }

    // ---- Additional Client tests ----

    #[test]
    fn client_new_creates_default() {
        let client = Client::new();
        assert!(client.base_url.is_none());
        let _ = client.inner();
    }

    #[test]
    fn client_builder_factory_method() {
        let client = Client::builder().base_url("https://example.com").build();
        assert_eq!(client.base_url.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn client_builder_factory_default() {
        let client = Client::builder().build();
        assert!(client.base_url.is_none());
    }

    // ---- Additional URL resolution tests ----

    #[test]
    fn url_resolution_empty_path_with_base() {
        let client = Client {
            base_url: Some("https://api.example.com".into()),
            inner: Client::new().inner,
        };
        let resolved = client.resolve_url("");
        assert_eq!(resolved, "https://api.example.com/");
    }

    #[test]
    fn url_resolution_base_multiple_trailing_slashes() {
        let client = Client {
            base_url: Some("https://api.example.com///".into()),
            inner: Client::new().inner,
        };
        let resolved = client.resolve_url("/users/1");
        assert_eq!(resolved, "https://api.example.com/users/1");
    }

    #[test]
    fn url_resolution_full_url_with_base() {
        let client = Client {
            base_url: Some("https://api.example.com".into()),
            inner: Client::new().inner,
        };
        let resolved = client.resolve_url("users/1");
        assert_eq!(resolved, "https://api.example.com/users/1");
    }

    #[test]
    fn url_resolution_deeply_nested_path() {
        let client = Client {
            base_url: Some("https://api.example.com".into()),
            inner: Client::new().inner,
        };
        let resolved = client.resolve_url("/v1/users/42/posts/7/comments");
        assert_eq!(
            resolved,
            "https://api.example.com/v1/users/42/posts/7/comments"
        );
    }

    // ---- Additional FetchError display tests ----

    #[test]
    fn fetch_error_network_empty_message() {
        let err = FetchError::Network("".into());
        let msg = err.to_string();
        assert!(msg.contains("network error"));
    }

    #[test]
    fn fetch_error_network_long_message() {
        let err = FetchError::Network("DNS resolution failed for api.example.com: NXDOMAIN".into());
        let msg = err.to_string();
        assert!(msg.contains("DNS resolution failed"));
        assert!(msg.contains("NXDOMAIN"));
    }

    #[test]
    fn fetch_error_timeout_zero_duration() {
        let err = FetchError::Timeout(Duration::from_secs(0));
        let msg = err.to_string();
        assert!(msg.contains("request timed out"));
        assert!(msg.contains("0ns"));
    }

    #[test]
    fn fetch_error_timeout_large_duration() {
        let err = FetchError::Timeout(Duration::from_secs(3600));
        let msg = err.to_string();
        assert!(msg.contains("request timed out"));
        assert!(msg.contains("3600s"));
    }

    #[test]
    fn fetch_error_status_code_various_codes() {
        for code in [400, 401, 403, 404, 429, 500, 502, 503] {
            let err = FetchError::StatusCode {
                status: code,
                body: format!("error {code}"),
            };
            let msg = err.to_string();
            assert!(msg.contains(&code.to_string()));
            assert!(msg.contains("HTTP"));
        }
    }

    #[test]
    fn fetch_error_status_code_empty_body() {
        let err = FetchError::StatusCode {
            status: 404,
            body: String::new(),
        };
        let msg = err.to_string();
        assert!(msg.contains("404"));
    }

    #[test]
    fn fetch_error_serialization_various_errors() {
        let json_err = serde_json::from_str::<serde_json::Value>("").unwrap_err();
        let err = FetchError::Serialization(json_err);
        let msg = err.to_string();
        assert!(msg.contains("serialization error"));
    }

    #[test]
    fn fetch_error_debug_format() {
        let errors = vec![
            FetchError::Network("test".into()),
            FetchError::Timeout(Duration::from_secs(1)),
            FetchError::StatusCode {
                status: 500,
                body: "err".into(),
            },
            FetchError::CircuitOpen,
            FetchError::Middleware("mw".into()),
        ];
        for err in errors {
            let debug_str = format!("{:?}", err);
            assert!(!debug_str.is_empty());
        }
    }

    #[test]
    fn fetch_error_is_std_error() {
        let err = FetchError::Network("test".into());
        let std_err: &dyn std::error::Error = &err;
        assert!(std_err.to_string().contains("network error"));
    }

    #[test]
    fn fetch_error_from_serde_json_roundtrip() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid json!!").unwrap_err();
        let msg = json_err.to_string();
        let fetch_err = FetchError::from(json_err);
        let fetch_msg = fetch_err.to_string();
        assert!(fetch_msg.contains("serialization error"));
        assert!(fetch_msg.contains(&msg));
    }

    // ---- Client clone test ----

    #[test]
    fn client_clone_preserves_base_url() {
        let client = Client::builder()
            .base_url("https://api.example.com")
            .build();
        let cloned = client.clone();
        assert_eq!(cloned.base_url.as_deref(), Some("https://api.example.com"));
    }

    #[test]
    fn client_clone_independence() {
        let client1 = Client::builder().base_url("https://first.com").build();
        let client2 = client1.clone();
        assert_eq!(client2.base_url.as_deref(), Some("https://first.com"));
    }

    // ---- New builder method tests ----

    #[test]
    fn client_builder_default_header() {
        let client = ClientBuilder::new()
            .default_header("x-custom", "value")
            .build();
        let _ = client.inner();
    }

    #[test]
    fn client_builder_user_agent() {
        let client = ClientBuilder::new().user_agent("fetch_kit/0.1").build();
        let _ = client.inner();
    }

    #[test]
    fn client_builder_retry_bounds() {
        let builder =
            ClientBuilder::new().retry_bounds(Duration::from_millis(100), Duration::from_secs(5));
        assert_eq!(builder.retries, 3);
        let client = builder.build();
        let _ = client.inner();
    }

    #[test]
    fn client_builder_try_build_success() {
        let result = ClientBuilder::new().try_build();
        assert!(result.is_ok());
    }

    #[test]
    fn fetch_error_build_error_display() {
        let err = FetchError::BuildError("tls error".into());
        let msg = err.to_string();
        assert!(msg.contains("build error"));
        assert!(msg.contains("tls error"));
    }

    #[test]
    fn fetch_error_header_error_display() {
        let err = FetchError::HeaderError("invalid header".into());
        let msg = err.to_string();
        assert!(msg.contains("header error"));
        assert!(msg.contains("invalid header"));
    }

    #[test]
    fn client_get_returns_request_builder() {
        let client = Client::new();
        let builder = client.get("https://example.com");
        let _ = builder;
    }

    #[test]
    fn client_post_returns_request_builder() {
        let client = Client::new();
        let builder = client.post("https://example.com");
        let _ = builder;
    }

    #[test]
    fn client_put_returns_request_builder() {
        let client = Client::new();
        let builder = client.put("https://example.com");
        let _ = builder;
    }

    #[test]
    fn client_delete_returns_request_builder() {
        let client = Client::new();
        let builder = client.delete("https://example.com");
        let _ = builder;
    }

    #[test]
    fn request_builder_header_chain() {
        let client = Client::new();
        let builder = client
            .get("https://example.com")
            .header("X-Test", "value")
            .bearer_auth("token");
        let _ = builder;
    }

    #[test]
    fn request_builder_query_chain() {
        let client = Client::new();
        let builder = client.get("https://example.com").query(&[("key", "value")]);
        let _ = builder;
    }

    #[test]
    fn request_builder_timeout_chain() {
        let client = Client::new();
        let builder = client
            .get("https://example.com")
            .timeout(Duration::from_secs(5));
        let _ = builder;
    }

    #[test]
    fn fetch_error_from_reqwest_timeout_uses_zero_duration() {
        let err = FetchError::Timeout(Duration::ZERO);
        assert!(err.to_string().contains("request timed out"));
        assert!(err.to_string().contains("0"));
    }
}
