# ADR 0033: Token response extensions and bounded failure reasons

## Status and evidence

Accepted before implementation. After ADR0032, owner testing reached the code-received browser acknowledgement, but Morons reported an invalid token response before credential installation. No token reply, credential, callback code/URL or browser state was collected. The exact failing validation branch is **unknown**; this increment does not claim to reconstruct it.

Two independent compatibility defects follow from [RFC6749 §5.1](https://www.rfc-editor.org/rfc/rfc6749#section-5.1): token type values are case-insensitive, and clients MUST ignore unrecognized response value names. The current decoder instead compares exactly `Bearer` and denies every unknown top-level field. The RFC's successful-response example explicitly contains `example_parameter`. Pinned public Codex459a79eb (`codex-rs/login/src/server.rs`) and OMP0fcdbb30 (`packages/ai/src/registry/oauth/openai-codex.ts`) consume known token fields without imposing that closed envelope. Their logging, loose duplicate handling, profile extraction, endpoint selection and fallback behavior are not adopted.

## Decision / changed boundaries

This explicitly supersedes only ADR0019's closed token envelope and case-sensitive Bearer spelling. It does not infer that every provider response or arbitrary OAuth variant is supported.

- Retain the fixed TLS endpoint, public client/redirect/grant and Morons attribution; no redirect, discovery, retry, credential import or token-paste API. Keep existing transport/header/body/time bounds, including64KiB token body and16KiB visible-ASCII access/refresh/optional ID tokens.
- Parse a duplicate-rejecting JSON tree, including unknown nested values, with the existing decoder recursion bound. The envelope must be an object with at most32 fields and nonempty names of at most128 UTF-8 bytes. Bounded unknown extension values are discarded, never used for routing, account selection, credentials, scopes, models, entitlement or policy. Numeric/type/malformed JSON and duplicate/escaped-alias rejection remain fail closed. Recognized RFC6749 §5.2 error-response members (`error`, `error_description`, `error_uri`) in a success envelope reject as contradictory rather than being treated as extensions. Unknown strings and keys join the existing zeroizing successful-tree cleanup; parser/allocator copies on failure remain a documented residual risk, not an erasure guarantee.
- A present token type must be ASCII-case-insensitively Bearer, with no whitespace normalization or alternate token scheme. Existing optionality and rejection of explicit null remain unchanged.
- Keep required access/refresh tokens and integer expires_in, exact returned scope-set validation, five-minute refresh margin,30-day lifetime ceiling, earlier-of-envelope/JWT expiry, bounded required access-token account claim and same-account refresh. Missing or differently shaped fields do not gain guessed defaults. Do not extend deadlines/lifetimes or accept broader scopes based on the generic owner error. ID token remains discarded, never an account fallback or identity assertion. Credential file formats and SQLite evidence do not change.

## Fixed diagnostics

Replace the undifferentiated invalid-response error with closed server-owned reasons: headers, body-bounds, body-framing, JSON, token-fields, token-type, scope, response-lifetime, access-token-format, claims-JSON, claim-expiry, account-claim, effective-lifetime, clock, or stored-credential. They describe only which local validation guard rejected. No remote field name/value, header, token, account, expiry, scope contents, parser error/source or provider body is attached, logged or persisted.

IPC43 carries an independent closed reason enum only in the initiating authenticated login connection's failed result. Provider errors and protocol DTOs remain separate, joined by an explicit server conversion. Old and new clients must negotiate matching protocol versions; no permissive fallback. The terminal displays a fixed reason and says existing credentials were not replaced and nothing was retried. It does not claim the provider is defective or that a remotely consumed code/refresh can be reused. The existing ordinary status, session history, durable failure identities and refresh reauthentication behavior remain unchanged; these reasons are not a new diagnostic credential-extraction endpoint.

## Qualification

First demonstrate failing-before synthetic extension/case variants. Add nested/escaped duplicates, field/name/body boundaries, discarded hostile metadata, unchanged scope/lifetime/account/secret checks, fixed reason redaction and exact closed-wire round trips. Exercise successful code exchange with extensions, invalid exchange with no installation/replay, and refresh rejection/no replay through existing custody tests. Run full locked local gates and exact-head platform/security CI before another deliberately owner-started login. Preserve prior failed samples outside Git; no real token inspection, automatic login retry, inference budget, retained-state upgrade, merge or release follows this repair.
