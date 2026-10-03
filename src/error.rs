/// Errors that can occur when making requests with `fetch_kit`.
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

    /// A middleware error raised inside the fetch-kit middleware chain.
    #[error("middleware error: {0}")]
    Middleware(String),

    /// Failed to build the client.
    #[error("build error: {0}")]
    BuildError(String),

    /// Failed to set a header.
    #[error("header error: {0}")]
    HeaderError(String),
}

impl From<reqwest::Error> for FetchError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            // The reqwest::Error does not expose the configured timeout duration,
            // so we use ZERO to indicate "the timeout triggered but the actual
            // duration is unknown at this point."
            FetchError::Timeout(std::time::Duration::ZERO)
        } else {
            FetchError::Network(err.to_string())
        }
    }
}

impl From<crate::middleware::Error> for FetchError {
    fn from(err: crate::middleware::Error) -> Self {
        match err {
            crate::middleware::Error::Middleware(e) => {
                // Preserve fetch_kit's own middleware errors (e.g.
                // `CircuitOpen`, `Timeout`) so callers can match on them.
                match e.downcast::<FetchError>() {
                    Ok(fetch_err) => *fetch_err,
                    Err(e) => FetchError::Middleware(e.to_string()),
                }
            }
            crate::middleware::Error::Reqwest(e) => FetchError::from(e),
        }
    }
}
