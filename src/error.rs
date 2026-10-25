/// Errors that can occur when making requests with `fetchkit`.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// A network-level error (DNS, connection refused, etc.).
    #[error("network error: {0}")]
    Network(String),

    /// The request timed out.
    #[error("request timed out after {0:?}")]
    Timeout(std::time::Duration),

    /// The server returned a non-success status code.
    #[error("HTTP {status}: {body}")]
    StatusCode {
        /// HTTP status code.
        status: u16,
        /// Response body.
        body: String,
    },

    /// Failed to serialize or deserialize JSON.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// The circuit breaker is open — requests are being rejected.
    #[error("circuit breaker is open; requests temporarily blocked")]
    CircuitOpen,

    /// A middleware error from reqwest-middleware.
    #[error("middleware error: {0}")]
    Middleware(String),
}

impl From<reqwest::Error> for FetchError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            FetchError::Timeout(std::time::Duration::from_secs(30))
        } else {
            FetchError::Network(err.to_string())
        }
    }
}

impl From<reqwest_middleware::Error> for FetchError {
    fn from(err: reqwest_middleware::Error) -> Self {
        match err {
            reqwest_middleware::Error::Middleware(e) => FetchError::Middleware(e.to_string()),
            reqwest_middleware::Error::Reqwest(e) => FetchError::from(e),
        }
    }
}
