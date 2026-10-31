# Security Policy

## Supported Versions

| Version | Supported |
|---|---|
| latest   | ✅ |
| < latest | ❌ (upgrade) |

## Reporting a Vulnerability

**Do not open a public issue.** Email **wyatt_au@protonmail.com** with:

1. Affected crate + version
2. A minimal reproduction or proof-of-concept
3. Impact assessment (what an attacker gains)
4. Suggested mitigation if known

You will receive an acknowledgment within 72 hours. Coordinated disclosure
window: 90 days from report, or earlier by mutual agreement. Reporters are
credited in the advisory unless anonymity is requested.

## Scope

In scope: anything that breaks the crate's documented security contract,
including panic on adversarial input in code paths documented as panic-free, signature/
MAC bypass, timing side channels in secret-dependent operations, injection
via parsed input, dependency-introduced vulnerabilities.

Out of scope: denial of service by resource exhaustion on already-documented
limits, vulnerabilities in downstream consumer code.

## Registry name collision (2026-10-31)

The crates.io package `fetchkit` (0.6.0, owner everruns) is NOT this project.
This crate publishes as `fetch-kit` (owner WyattAu; repository
https://github.com/WyattAu/fetch-kit). A same-shaped-name package for an
agent-fetching tool is exactly the impersonation pattern that hit peers in
this space (see morluto/rea's trojanized 6.3.0 archive). Verify the owner
before installing; our release channel is tags on the repository above.
