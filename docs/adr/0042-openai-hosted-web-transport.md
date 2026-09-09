# ADR 0042: One-shot server-owned hosted-search transport

## Status
Accepted before implementation, 2026-09-08. Focused transport increment after ADR0041. No application/default admission, new IPC or persistence migration yet; Brave remains active until durable root/task bindings and presentation are implemented.

## Decision
Use the existing private OpenAI credential broker and bounded HTTP client for one exact reviewed ChatGPT Codex Responses route. A private registration binds each search attempt to its provider instance, immutable query request, nonzero operation identity and exact owner credential generation. Encoding is not authorization: the application must later supply committed search bindings and current policy admission. Check the reviewed unknown native data-use policy before/after credential preparation and again before dispatch.

A prepared lease is server-only. Send Morons attribution, source-separated opaque operation/generation request identifiers and sensitive broker access/account headers. Never send refresh tokens or account data in the request body, use browser cookies or other-app credentials, accept a production endpoint override, or log query/raw response/header/credential values. Release the credential lease after headers. Search is one-shot: mark the attempt spent before HTTP starts, and never reset it after success, failure, cancellation or drop. A new attempt is not permission to replay an uncertain persisted operation; later storage admission remains authoritative.

Reuse existing finite connect/header/idle bounds and status/header/framing helpers, with a fixed120-second total response deadline. Require HTTP200; permit absent native Content-Type only under ADR0037, reject present incompatible/duplicate framing headers. Bound body to ADR0041's256KiB and decode only its complete hosted-search SSE contract. No sniffing, redirect, compression fallback, retry, model substitution, sticky routing or cross-request reasoning memory. Errors are existing fixed ProviderError classifications, not remote text. Deadlines and local byte/token/action bounds do not establish remote cancellation or spending caps.

## Validation / deferred admission
Synthetic broker/loopback tests verify exact route/body/headers, instance/generation/policy boundaries, one-shot success/rejection/cancel/deadline behavior and no second request. No real credential or paid request is used. Run all locked local and exact-head platform/security gates. Root/task durable source/model/credential binding, nested search usage/disclosure, explicit default configuration and safe source presentation remain prerequisites before live application qualification.
