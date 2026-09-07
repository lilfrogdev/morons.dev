# ADR 0032: OAuth callback extensions and safe rejection reasons

## Status and evidence

Accepted 2026-09-07 before implementation, after owner browser testing returned `Callback rejected. Return to Morons.` No callback URL, code, token, browser history or credential file was collected. That old static response conflated HTTP/query/state rejection with a valid provider denial; the exact live cause is therefore **unproven**.

A separate deterministic compatibility defect is established by the code: every query field except code/state/error/error_description rejects, and decoding accepts only form-escaped or unreserved literal characters. [RFC 6749 §4.1.2](https://www.rfc-editor.org/rfc/rfc6749#section-4.1.2) explicitly requires clients to ignore unrecognized authorization-response parameters. [RFC 3986 §3.4](https://www.rfc-editor.org/rfc/rfc3986#section-3.4) permits additional literal query characters. Pinned public Codex459a79eb (`codex-rs/login/src/server.rs`) and OpenCode5cf9f517 (`packages/opencode/src/plugin/openai/codex.ts`) consume code/state from a general query parser, not the former four-name allowlist. Their logging, duplicate behavior, import/API-key paths and alternate transport behavior are not adopted.

[RFC 9207 §2.4](https://www.rfc-editor.org/rfc/rfc9207#section-2.4) requires an understood optional `iss` to be compared exactly. Public OpenAI discovery metadata reviewed 2026-09-07 at `https://auth.openai.com/.well-known/openid-configuration` reports issuer `https://auth.openai.com` and query/code responses. Only this reviewed issuer value is pinned; **no endpoint from remote metadata is imported or followed**. Existing fixed authorize/token/resource routes and Morons attribution remain unchanged.

## Decision / changed boundary

Replace the closed four-field callback grammar with bounded authorization-code response parsing. This deliberately supersedes ADR0019's rejection of all unknown callback query fields; it does not relax application IPC or token-response decoding.

- Keep 8KiB whole request,32headers,16connections,5s per connection and600s whole login. Permit at most16query fields, names1..128decoded ASCII bytes and values at most4096decoded ASCII bytes, all within the unchanged total request bound.
- Decode form percent escapes and plus, accepting the legal ASCII query component literals from RFC3986. Do not accept invalid escapes, control/non-ASCII bytes, fragments, backslashes or raw whitespace. Encoded and literal spellings have the same decoded meaning; this is not a larger code alphabet.
- Reject every duplicate decoded name, including unknown metadata and encoded aliases. HTTP method/version/path/host, no-body/transfer/Origin checks remain unchanged. State remains required and exactly constant-time matched before any code is returned. Missing/empty/oversized code, code/error contradictions and unsupported token-bearing hybrid responses reject.
- Unknown response extensions (including scope/session metadata) are decoded within bounds, then discarded. They never choose a route, redirect, credential/account, entitlement, model, favorable policy, scope grant or token request field. The fixed TLS code exchange and its existing strict token/scope decoder alone establish usable credential material. No callback token is installed or imported.
- If `iss` is present, it must equal the fixed reviewed issuer exactly; no URL normalization, redirect, remote selection or signature/identity claim. Absence remains supported because this one fixed issuer's native compatibility contract did not require the extension. A mismatch rejects success and error responses.
- Rejected requests still cannot consume a valid pending code or extend the deadline; denial with matching state is terminal and has its own fixed browser message. Received-code acknowledgement still says login is incomplete until durable installation. No automatic login/code/exchange retry.

## Diagnostics

When a response can be sent within the unchanged connection deadline, return only fixed browser-local classifications: request format/bounds/timeout/incomplete, headers, host, path, query format, duplicate field, state, issuer, or response shape. No request target, raw field name/value, header, state, code, token or parser error is reflected, logged, placed in IPC/session history or persisted. Do not claim one classification proves provider identity or browser execution. These reasons make a future owner report actionable without asking for sensitive callback data.

## Qualification

Add failing-before synthetic browser-style callbacks with extensions and legal query literals, fixed issuer checking, duplicate/host/state/body/size/metadata bounds, secret-free reason output and denial separation. Loopback end-to-end tests must demonstrate no token request for a rejected callback, a later valid callback still accepted, exactly the existing five exchange fields and one exchange, with no metadata route/policy influence or replay. Keep existing hostile callback and token tests. Run full local and exact-head platform/security gates before a new owner-authorized login attempt. Preserve this failed sample outside Git and keep its exact cause unproven unless later evidence establishes it.
