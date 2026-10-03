# Requirements — fetchkit

Numbered, testable requirements. Every requirement maps to at least one named
test or doc-comment contract; security-relevant items cite THREAT-MODEL.md rows.

Scope: Resilient HTTP client — native middleware stack (reqwest-middleware-compatible trait shape) with retries, timeouts, throttling, and circuit breaking

## Functional

| ID | Requirement | Priority |
|----|-------------|----------|
| REQ-FK-001 | Retries apply only to transient failures with exponential backoff + jitter; non-retryable statuses surface immediately | MUST |
| REQ-FK-002 | Circuit breaker opens after the configured failure threshold and half-opens after the cooldown | MUST |
| REQ-FK-003 | Per-request timeouts are enforced; no unbounded waits | MUST |
| REQ-FK-004 | The `Middleware` trait composes as an onion with documented ordering: first-registered = outermost; proven by an ordering test | MUST |
| REQ-FK-005 | Request-scoped extensions (`with_extension<T>`) are visible to every middleware and survive retry attempts | MUST |
| REQ-FK-006 | `ThrottleMiddleware` rate-bounds sustained throughput per authority (proptest: elapsed ≥ (n−burst)/rate) | MUST |
| REQ-FK-007 | Retry middleware never retries middleware errors, so breaker short-circuits surface as `FetchError::CircuitOpen` | MUST |
| REQ-FK-008 | `BaseUrlMiddleware` rewrites only marker-host requests; absolute URLs pass through untouched | MUST |

## Security

| ID | Requirement | Priority |
|----|-------------|----------|
| REQ-FK-100 | TLS verification is never disabled by the crate (no dangerous-config constructors) | MUST |
| REQ-FK-101 | Response bodies are bounded by configured limits before buffering | SHOULD |

## Observability & API hygiene

| ID | Requirement | Priority |
|----|-------------|----------|
| REQ-FK-900 | All fallible public APIs return typed errors; production `unwrap`/`expect` is denied or explicitly justified with an invariant comment | MUST |
| REQ-FK-901 | Public items carry doc comments with runnable examples where practical | SHOULD |

Reviewed: 2026-09-11
