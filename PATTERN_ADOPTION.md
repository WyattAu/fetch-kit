# PATTERN_ADOPTION: fetch-kit

Relationship to the WyattAu pattern canon (`references/catalog.md` in
pattern-standards). Rows are code-verified; checked by
`pattern-standards/scripts/adoption-check.py`.

| Pattern | Status | Evidence |
|---|---|---|
| T1 | adopted | src/lib.rs:340 (get_json/post_json parse the response into the caller's type at the call boundary; failures surface as FetchError, never as re-validated strings) |
| P51 | adopted | src/middleware/circuit_breaker.rs:44 (open circuit short-circuits before the transport), src/middleware/circuit_breaker.rs:54 (429/5xx and network errors record failures), wired into the default chain at src/lib.rs:241 |
| P52 | partial | src/middleware/throttle.rs:69 (per-authority token buckets isolate per-host request rate; the reqwest connection pool stays shared across hosts, src/middleware/chain.rs:124) |
| P64 | adopted | tests/proptest.rs:7 (builder-config property suite in push CI; no scheduled enlarged-case soak yet) |
| P24 | refused | R6 (retry amplification is bounded today by fixed per-request caps, src/lib.rs:232, and breaker short-circuits are never retried, src/middleware/retry.rs:66; a cross-request success-refilled budget has no failure mode in scope yet) |
| P104 | refused | R6 (resolution is delegated to reqwest defaults; callers override through the raw builder passthrough, src/lib.rs:183; no resolver pinning or pool tuning in this crate) |
