//! The core middleware chain: the [`Middleware`] trait, the [`Next`]
//! continuation, and the [`ClientWithMiddleware`] runner.
//!
//! The shape intentionally mirrors `reqwest-middleware` 0.5 so that
//! migrating an existing middleware is mechanical: swap the import path
//! from `reqwest_middleware::{Middleware, Next, ...}` to
//! `fetch_kit::middleware::{Middleware, Next, ...}` and the signature is
//! unchanged.
//!
//! # Ordering semantics
//!
//! Middleware form an onion around the transport. The middleware
//! registered **first** is the **outermost** layer: it observes the
//! request first and the response last.
//!
//! ```text
//! ClientBuilder::new(client)
//!     .with(AuthMiddleware)      // 1st registered → outermost
//!     .with(RetryMiddleware)     // 2nd registered
//!     .build();
//!
//! request  →  Auth  →  Retry  →  transport
//! response ←  Auth  ←  Retry  ←  ╹
//! ```

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use http::Extensions;
use reqwest::{Method, Request, Response};

/// Result alias used throughout the middleware stack.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Error type produced by the middleware stack.
///
/// Mirrors `reqwest-middleware`'s error: either the underlying
/// `reqwest::Error`, or an opaque error raised by a middleware
/// (e.g. [`crate::FetchError::CircuitOpen`]).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A middleware produced an error.
    #[error("middleware failed: {0}")]
    Middleware(#[from] Box<dyn std::error::Error + Send + Sync>),

    /// The underlying `reqwest` client produced an error.
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
}

/// Async middleware that wraps a [`ClientWithMiddleware`].
///
/// Implement `handle` to inspect or rewrite the request before it travels
/// inward, and/or the response (or error) on the way back out. Call
/// [`Next::run`] to continue the chain; returning without calling it
/// short-circuits everything inward (including the transport).
///
/// The `extensions` argument is a per-logical-request typed map shared by
/// every middleware in the chain (and across all retry attempts of one
/// logical request); use it for cross-middleware state. Request-scoped
/// typed data set via [`RequestBuilder::with_extension`] arrives in this
/// map.
#[async_trait]
pub trait Middleware: 'static + Send + Sync {
    /// Handle the request, optionally calling `next` to continue the chain.
    async fn handle(
        &self,
        req: Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> Result<Response>;
}

/// The continuation passed to [`Middleware::handle`].
///
/// `run` executes the remainder of the chain (the middleware registered
/// after this one, then the transport). `Next` is cheap to clone, which is
/// how retry-style middleware issue multiple attempts.
#[derive(Clone)]
pub struct Next<'a> {
    client: &'a ClientWithMiddleware,
    remaining: &'a [Arc<dyn Middleware>],
}

impl Next<'_> {
    pub(crate) fn new<'a>(
        client: &'a ClientWithMiddleware,
        remaining: &'a [Arc<dyn Middleware>],
    ) -> Next<'a> {
        Next { client, remaining }
    }

    /// Continue the chain with the (possibly rewritten) request.
    pub fn run(
        self,
        req: Request,
        extensions: &mut Extensions,
    ) -> impl Future<Output = Result<Response>> {
        let client = self.client;
        let remaining = self.remaining;
        async move { execute_with_chain(client, remaining, req, extensions).await }
    }
}

/// Execute `req` through `middleware` and finally `client`.
pub(crate) async fn execute_with_chain(
    client: &ClientWithMiddleware,
    middleware: &[Arc<dyn Middleware>],
    req: Request,
    extensions: &mut Extensions,
) -> Result<Response> {
    if let Some((current, rest)) = middleware.split_first() {
        current
            .handle(req, extensions, Next::new(client, rest))
            .await
    } else {
        client.client.execute(req).await.map_err(Error::Reqwest)
    }
}

/// A `reqwest::Client` wrapped in a middleware chain.
///
/// Cloneable (all clones share the same chain and connection pool) and
/// `Debug`. Built with [`ClientBuilder`].
#[derive(Clone)]
pub struct ClientWithMiddleware {
    client: reqwest::Client,
    middleware: Arc<[Arc<dyn Middleware>]>,
}

impl ClientWithMiddleware {
    pub(crate) fn new(client: reqwest::Client, middleware: Arc<[Arc<dyn Middleware>]>) -> Self {
        Self { client, middleware }
    }

    /// The number of middleware registered on this client.
    pub fn middleware_count(&self) -> usize {
        self.middleware.len()
    }

    /// Return a new client with `middleware` appended **inside** the
    /// existing chain (closest to the transport).
    ///
    /// This is a composition point for clients that were already built
    /// (e.g. [`crate::Client::inner`]). When building from scratch, prefer
    /// [`ClientBuilder::with`], which registers middleware outermost-first.
    pub fn with_middleware(mut self, middleware: Arc<dyn Middleware>) -> Self {
        let mut mws = Vec::with_capacity(self.middleware.len() + 1);
        mws.extend(self.middleware.iter().cloned());
        mws.push(middleware);
        self.middleware = mws.into();
        self
    }

    /// Execute the request through the middleware chain with a fresh,
    /// empty per-request extension map.
    pub async fn execute(&self, req: Request) -> Result<Response> {
        let mut extensions = Extensions::new();
        self.execute_with_extensions(req, &mut extensions).await
    }

    /// Execute the request through the middleware chain, seeding the
    /// per-request extension map with `extensions`.
    ///
    /// The map is shared by every middleware in the chain and survives
    /// across retry attempts of the same logical request; it is the
    /// request-scoped typed map that [`RequestBuilder::with_extension`]
    /// populates.
    pub async fn execute_with_extensions(
        &self,
        req: Request,
        extensions: &mut Extensions,
    ) -> Result<Response> {
        execute_with_chain(self, &self.middleware, req, extensions).await
    }

    /// Convenience method to make a `GET` request to a URL.
    pub fn get(&self, url: impl Into<String>) -> RequestBuilder {
        self.request(Method::GET, url)
    }

    /// Convenience method to make a `POST` request to a URL.
    pub fn post(&self, url: impl Into<String>) -> RequestBuilder {
        self.request(Method::POST, url)
    }

    /// Convenience method to make a `PUT` request to a URL.
    pub fn put(&self, url: impl Into<String>) -> RequestBuilder {
        self.request(Method::PUT, url)
    }

    /// Convenience method to make a `PATCH` request to a URL.
    pub fn patch(&self, url: impl Into<String>) -> RequestBuilder {
        self.request(Method::PATCH, url)
    }

    /// Convenience method to make a `HEAD` request to a URL.
    pub fn head(&self, url: impl Into<String>) -> RequestBuilder {
        self.request(Method::HEAD, url)
    }

    /// Convenience method to make a `DELETE` request to a URL.
    pub fn delete(&self, url: impl Into<String>) -> RequestBuilder {
        self.request(Method::DELETE, url)
    }

    /// Start building a request with the given method and URL.
    pub fn request(&self, method: Method, url: impl Into<String>) -> RequestBuilder {
        RequestBuilder {
            client: self.clone(),
            inner: self.client.request(method, url.into()),
            extensions: Vec::new(),
        }
    }
}

impl std::fmt::Debug for ClientWithMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientWithMiddleware")
            .field("client", &self.client)
            .field("middleware", &self.middleware.len())
            .finish()
    }
}

/// Builder for [`ClientWithMiddleware`]: wrap a `reqwest::Client` with
/// middleware, **first-registered = outermost**.
///
/// Mirrors `reqwest-middleware`'s `ClientBuilder` so migration is
/// mechanical.
#[derive(Clone)]
pub struct ClientBuilder {
    client: reqwest::Client,
    middleware: Vec<Arc<dyn Middleware>>,
}

impl ClientBuilder {
    /// Wrap the given `reqwest::Client` with a middleware chain.
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            middleware: Vec::new(),
        }
    }

    /// Register a middleware. The first middleware registered runs
    /// outermost (sees the request first, the response last).
    pub fn with<M: Middleware>(self, middleware: M) -> Self {
        self.with_middleware(Arc::new(middleware))
    }

    /// Register an already-erased middleware (same ordering as
    /// [`ClientBuilder::with`]).
    pub fn with_middleware(mut self, middleware: Arc<dyn Middleware>) -> Self {
        self.middleware.push(middleware);
        self
    }

    /// Build the [`ClientWithMiddleware`].
    pub fn build(self) -> ClientWithMiddleware {
        ClientWithMiddleware::new(self.client, self.middleware.into())
    }
}

/// One registered "insert this typed value into the request-scoped
/// extensions map" operation, applied when the request is sent.
type ExtensionApplier = Arc<dyn Fn(&mut Extensions) + Send + Sync>;

/// A request builder bound to a [`ClientWithMiddleware`].
///
/// Wraps `reqwest::RequestBuilder` and adds
/// [`RequestBuilder::with_extension`] for request-scoped typed data that
/// every middleware can read (`extensions.get::<T>()` in
/// [`Middleware::handle`]).
///
/// Not `Clone` (reqwest's builder isn't either); use
/// [`RequestBuilder::try_clone`] for bufferable requests.
pub struct RequestBuilder {
    client: ClientWithMiddleware,
    inner: reqwest::RequestBuilder,
    extensions: Vec<ExtensionApplier>,
}

impl RequestBuilder {
    /// Attach request-scoped typed data, visible to every middleware in
    /// the chain through the `extensions` argument of
    /// [`Middleware::handle`] (`extensions.get::<T>()`). The map is shared
    /// across retry attempts of the same logical request, so the data
    /// survives retries.
    pub fn with_extension<T: Clone + Send + Sync + 'static>(mut self, value: T) -> Self {
        self.extensions.push(Arc::new(move |ext: &mut Extensions| {
            ext.insert(value.clone());
        }));
        self
    }

    /// Add a single header.
    pub fn header<K, V>(mut self, key: K, value: V) -> Self
    where
        http::header::HeaderName: TryFrom<K>,
        <http::header::HeaderName as TryFrom<K>>::Error: Into<http::Error>,
        http::HeaderValue: TryFrom<V>,
        <http::HeaderValue as TryFrom<V>>::Error: Into<http::Error>,
    {
        self.inner = self.inner.header(key, value);
        self
    }

    /// Set all headers from a `HeaderMap`.
    pub fn headers(mut self, headers: reqwest::header::HeaderMap) -> Self {
        self.inner = self.inner.headers(headers);
        self
    }

    /// Append query parameters.
    pub fn query<T: serde::Serialize + ?Sized>(mut self, params: &T) -> Self {
        self.inner = self.inner.query(params);
        self
    }

    /// Override the request timeout.
    pub fn timeout(mut self, timeout: std::time::Duration) -> Self {
        self.inner = self.inner.timeout(timeout);
        self
    }

    /// Set a JSON body (requires the `json` feature).
    #[cfg(feature = "json")]
    pub fn json<T: serde::Serialize + ?Sized>(mut self, body: &T) -> Self {
        self.inner = self.inner.json(body);
        self
    }

    /// Set a `Bearer` authorization token.
    pub fn bearer_auth(mut self, token: impl std::fmt::Display) -> Self {
        self.inner = self.inner.bearer_auth(token);
        self
    }

    /// Set HTTP basic auth credentials.
    pub fn basic_auth<U, P>(mut self, username: U, password: Option<P>) -> Self
    where
        U: std::fmt::Display,
        P: std::fmt::Display,
    {
        self.inner = self.inner.basic_auth(username, password);
        self
    }

    /// Set the request body directly.
    pub fn body(mut self, body: impl Into<reqwest::Body>) -> Self {
        self.inner = self.inner.body(body);
        self
    }

    /// Set form-encoded body from a serializable value.
    pub fn form<T: serde::Serialize + ?Sized>(mut self, form: &T) -> Self {
        self.inner = self.inner.form(form);
        self
    }

    /// Set a multipart form body (requires the `multipart` feature).
    #[cfg(feature = "multipart")]
    pub fn multipart(mut self, form: reqwest::multipart::Form) -> Self {
        self.inner = self.inner.multipart(form);
        self
    }

    /// Build the request without sending it.
    ///
    /// Extension values registered via [`RequestBuilder::with_extension`]
    /// are applied by [`RequestBuilder::send`] (they seed the per-request
    /// extension map passed to middleware, which reqwest's `Request` type
    /// does not expose).
    pub fn build(self) -> Result<Request> {
        Ok(self.inner.build()?)
    }

    /// Clone the builder, if the underlying request is cloneable
    /// (buffered bodies are; streaming bodies are not).
    pub fn try_clone(&self) -> Option<Self> {
        Some(Self {
            client: self.client.clone(),
            inner: self.inner.try_clone()?,
            extensions: self.extensions.clone(),
        })
    }

    /// Build and execute the request through the middleware chain,
    /// seeding the per-request extension map with any
    /// [`RequestBuilder::with_extension`] values.
    pub async fn send(self) -> Result<Response> {
        let Self {
            client,
            inner,
            extensions: appliers,
        } = self;
        let request = inner.build()?;
        let mut extensions = Extensions::new();
        for apply in &appliers {
            apply(&mut extensions);
        }
        client
            .execute_with_extensions(request, &mut extensions)
            .await
    }
}
