//! Base-URL rewriting middleware — prefixes a base URL onto relative
//! request paths.
//!
//! `reqwest` requires `http`/`https` URLs, so relative requests are
//! expressed with a reserved marker host: [`RELATIVE_MARKER_HOST`]
//! (`fetch-kit.relative`, not a routable TLD). Build such a request with
//! [`BaseUrlMiddleware::relative`] and register this middleware; every
//! request aimed at the marker host is rewritten onto the base URL (path
//! joined, query preserved). Requests aimed at any other host pass
//! through untouched.
//!
//! ```text
//! base = https://api.example.com/v1/
//! http://fetch-kit.relative/users/1?x=2 → https://api.example.com/v1/users/1?x=2
//! ```
//!
//! This is the middleware-level counterpart of
//! [`crate::ClientBuilder::base_url`] (which resolves eagerly at the
//! `fetch_kit::Client` layer). Requires the `base-url` feature.

use async_trait::async_trait;
use http::Extensions;
use reqwest::{Request, Response, Url};

use super::{Middleware, Next, Result};

/// Reserved marker host marking a relative request URL for
/// [`BaseUrlMiddleware`] (`fetch-kit.relative` — `.relative` is not a
/// routable TLD, so the marker cannot collide with a real origin).
pub const RELATIVE_MARKER_HOST: &str = "fetch-kit.relative";

/// Base-URL prefixing middleware.
///
/// Rewrites requests aimed at [`RELATIVE_MARKER_HOST`] onto the
/// configured base URL. Path joining mirrors the `base_url` builder knob:
/// trailing slashes on the base path and leading slashes on the request
/// path are normalized so `…/v1/` + `/users/1` == `…/v1` + `users/1`.
#[derive(Debug, Clone)]
pub struct BaseUrlMiddleware {
    base: Url,
}

impl BaseUrlMiddleware {
    /// Create a middleware prefixing `base` onto relative requests.
    pub fn new(base: Url) -> Self {
        Self { base }
    }

    /// Build a relative request URL carrying `path` (leading slash
    /// optional; query string allowed, e.g. `/users?active=true`).
    pub fn relative(path: &str) -> Result<Url, url::ParseError> {
        let trimmed = path.trim_start_matches('/');
        Url::parse(&format!("http://{RELATIVE_MARKER_HOST}/{trimmed}"))
    }

    /// Join a relative request URL onto the base URL: the request's path
    /// and query are kept, the scheme/host/port/credentials come from the
    /// base, and any base-level query is replaced by the request's.
    fn join(&self, relative: &Url) -> Url {
        let req_path = relative.path().trim_start_matches('/');
        let base_path = self.base.path().trim_end_matches('/');
        let mut joined_url = self.base.clone();
        joined_url.set_path(&format!("{base_path}/{req_path}"));
        joined_url.set_query(relative.query());
        joined_url
    }
}

#[async_trait]
impl Middleware for BaseUrlMiddleware {
    async fn handle(
        &self,
        mut req: Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> Result<Response> {
        if req.url().host_str() == Some(RELATIVE_MARKER_HOST) {
            let joined = self.join(req.url());
            *req.url_mut() = joined;
        }
        next.run(req, extensions).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // test assertions unwrap by design
    use super::*;

    fn mw(base: &str) -> BaseUrlMiddleware {
        BaseUrlMiddleware::new(Url::parse(base).unwrap())
    }

    #[test]
    fn relative_helper_builds_marker_host() {
        let url = BaseUrlMiddleware::relative("/users/1").unwrap();
        assert_eq!(url.host_str(), Some(RELATIVE_MARKER_HOST));
        assert_eq!(url.path(), "/users/1");
    }

    #[test]
    fn relative_helper_keeps_query() {
        let url = BaseUrlMiddleware::relative("users?active=true").unwrap();
        assert_eq!(url.path(), "/users");
        assert_eq!(url.query(), Some("active=true"));
    }

    #[test]
    fn join_prefixes_base_path() {
        let middleware = mw("https://api.example.com/v1/");
        let joined = middleware.join(&BaseUrlMiddleware::relative("/users/1").unwrap());
        assert_eq!(joined.as_str(), "https://api.example.com/v1/users/1");
    }

    #[test]
    fn join_normalizes_slashes() {
        let middleware = mw("https://api.example.com");
        let joined = middleware.join(&BaseUrlMiddleware::relative("users/1").unwrap());
        assert_eq!(joined.as_str(), "https://api.example.com/users/1");
    }

    #[test]
    fn join_preserves_query_and_replaces_base_query() {
        let middleware = mw("https://api.example.com/v1?env=dev");
        let joined = middleware.join(&BaseUrlMiddleware::relative("/users?q=2").unwrap());
        assert_eq!(joined.as_str(), "https://api.example.com/v1/users?q=2");
        assert_eq!(joined.query(), Some("q=2"));
    }

    #[test]
    fn join_empty_path_lands_on_base_root() {
        let middleware = mw("https://api.example.com/v1/");
        let joined = middleware.join(&BaseUrlMiddleware::relative("").unwrap());
        assert_eq!(joined.as_str(), "https://api.example.com/v1/");
    }

    #[test]
    fn join_carries_base_credentials_and_port() {
        let middleware = mw("http://user:pw@127.0.0.1:8080/api");
        let joined = middleware.join(&BaseUrlMiddleware::relative("/x").unwrap());
        assert_eq!(joined.as_str(), "http://user:pw@127.0.0.1:8080/api/x");
    }

    #[test]
    fn relative_helper_percent_encodes_rather_than_panicking() {
        // The fixed placeholder scheme means Url::parse can always accept
        // the input; odd characters are percent-encoded.
        let url = BaseUrlMiddleware::relative("a b/c?d=1").unwrap();
        assert_eq!(url.path(), "/a%20b/c");
        assert_eq!(url.query(), Some("d=1"));
    }
}
