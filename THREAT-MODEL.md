# Threat Model: fetchkit

Reference: STRIDE. Scope: the crate's public API surface (`Client`,
`ClientBuilder`, `RequestBuilder`, `FetchError`) as used by a downstream
service. Trust boundaries: (1) URLs and header strings entering builder
methods, (2) responses (status, body, headers) crossing back into the
caller's error handling, (3) the dependency tree (reqwest, reqwest-middleware,
reqwest-retry, rustls/native-tls per feature).

## Assets

| ID | Asset | Example |
|----|-------|---------|
| A1 | Confidentiality of credentials passed to requests | Bearer token or basic-auth password leaked into logs |
| A2 | Integrity of request destination (no unintended hosts) | Base-URL confusion sends an authenticated request to the wrong host |
| A3 | Availability of the caller (no hangs, no unbounded retries) | Wedged peer or retry storm ties up worker tasks |

## STRIDE Analysis

| # | Threat | Category | Surface | Mitigation | Verifying test |
|---|--------|----------|---------|------------|----------------|
| T1 | Requests hang forever on a wedged peer | DoS | all request methods | Default 30 s total timeout on every built client; per-request override via `RequestBuilder::timeout`; retries capped (default 3) with bounded exponential backoff (`retry_bounds`) | `client_builder_default_timeout`, `client_builder_default_retries`, `client_builder_retry_bounds`, `client_builder_timeout_zero` |
| T2 | Base-URL confusion / open redirect into credentials | Spoofing | `resolve_url`, `base_url` | Resolution is plain concatenation: base is trimmed of trailing `/`, path of leading `/`; a *full* URL passed with a base set is still appended to the base (never a bare redirect to an attacker host). No automatic cross-host following beyond reqwest defaults | `url_resolution_base_with_relative_path`, `url_resolution_full_url_with_base`, `url_resolution_base_multiple_trailing_slashes`, `url_resolution_empty_path_with_base` |
| T3 | Credential leakage through error values | Info disclosure | `FetchError` | **Not mitigated**: `FetchError::StatusCode` embeds the raw response body and `Network` embeds the underlying message; `Debug`/`Display` render them fully. Credentials themselves are never stored by the client (headers live only in the request), so the crate cannot leak them, but caller bodies echoed in errors may be sensitive. Documented residual risk | `fetch_error_status_code_display`, `fetch_error_debug_format` (assert full rendering, i.e. no redaction) |
| T4 | Invalid header/URL input panics | DoS | `ClientBuilder::default_header`, `build()` | Documented, deliberate panics: `default_header` rejects invalid `&'static str` at construction (caller programming error); `build()` documents its panic as process-startup defect and `try_build()` is the fallible alternative | `client_builder_default_header` (valid path); `client_builder_try_build_success`; panic contracts documented on both methods |
| T5 | Plaintext/HTTP downgrade | Elevation | `reqwest_builder` escape hatch | **Not mitigated**: the raw `reqwest::ClientBuilder` passthrough lets callers disable TLS or relax defaults; no allow-list of schemes. Documented residual risk: TLS policy is caller-owned | Code review |
| T6 | Fallback sends the request to the wrong endpoint on transient failure | Tampering | `fetch_with_fallback` | Documented semantics: *any* primary failure (including 4xx) triggers the fallback fetch; the fallback result is authoritative. No signature binding between primary and fallback responses | Not covered by a dedicated test: documented residual risk |

## Repudiation

The crate performs no logging and keeps no audit trail (stateless wrapper).
Request/response evidence lives entirely in the caller's stack. Accepted.
A client library is the wrong layer for audit.

## Out of Scope

- TLS configuration, certificate pinning, and proxy policy: delegated to
  reqwest via `reqwest_builder`.
- Response body size limits: `json()`/`text()` will buffer whatever the
  server sends; hostile-server memory exhaustion is not defended here.
- SSRF policy: whether a URL is safe to request is a caller decision; the
  crate resolves strings, it does not judge destinations.
- Auth token lifetime/refresh: `bearer_auth` takes any `Display` and
  attaches it verbatim.

## Residual Risks

- **R1 (Medium, accepted):** Error values echo server-controlled bodies
  (T3). A malicious server can inject log-forging content (newlines, ANSI)
  into `FetchError` rendering. Callers should treat `FetchError` text as
  untrusted in logs.
- **R2 (Low, accepted):** Naive base/path concatenation (T2) means a path
  like `../admin` is passed through unnormalized: no dot-segment removal.
  Callers building URLs from user input must normalize first.
- **R3 (Low, accepted):** Retry defaults apply to any transient middleware
  classification; a flaky authenticated endpoint receives up to 4 requests.
  `retries(0)` is available and tested (`client_builder_retries_zero`).
- **R4 (Low, accepted):** Dependency risk in reqwest/hyper stack; no
  in-repo `cargo audit` gate (org-level Dependabot only).
