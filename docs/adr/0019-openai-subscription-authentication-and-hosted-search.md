# ADR 0019: OpenAI subscription authentication and hosted search

- Status: Proposed — implementation blocked on the approval gates below
- Date: 2026-09-04

## Context

Morons currently stores one server-owned OpenCode API key and sends `web_search` queries to Brave Search using the ordinary `BRAVE_SEARCH_API_KEY` environment variable. The intended replacement is OpenAI-hosted web search authenticated by a user's ChatGPT subscription, without reading or reusing credentials owned by Pi, Codex, a browser profile, or another application.

This is not just a search-adapter change. It introduces a second managed credential authority, browser authentication, rotating access and refresh tokens, an unauthenticated loopback callback, account-scoped request headers, another billable model request, token revocation, and policy-dependent data use. Those boundaries must be settled before any endpoint, client identity, token format, or hosted-search request is admitted.

### Researched contracts

OpenAI's [Codex authentication documentation](https://developers.openai.com/codex/auth) documents ChatGPT subscription login for the ChatGPT desktop app, Codex CLI, and IDE extension. It says ChatGPT-authenticated use follows ChatGPT workspace permissions, retention, residency, and data controls; tokens are refreshed automatically; logout clears local credentials; and file storage contains sensitive plaintext tokens.

At OpenAI Codex commit [`8e6a44b`](https://github.com/openai/codex/tree/8e6a44b428e31f91b21edc97904fcdf4f0931ade), the official client:

- creates a loopback authorization-code flow with S256 PKCE and random state in [`codex-rs/login/src/server.rs`](https://github.com/openai/codex/blob/8e6a44b428e31f91b21edc97904fcdf4f0931ade/codex-rs/login/src/server.rs);
- exchanges and refreshes tokens through OpenAI auth endpoints, serializes refreshes, preserves rotated refresh tokens, and distinguishes permanent refresh-token failures in [`codex-rs/login/src/auth/manager.rs`](https://github.com/openai/codex/blob/8e6a44b428e31f91b21edc97904fcdf4f0931ade/codex-rs/login/src/auth/manager.rs); and
- attempts one best-effort token revocation while still clearing local state in [`codex-rs/login/src/auth/revoke.rs`](https://github.com/openai/codex/blob/8e6a44b428e31f91b21edc97904fcdf4f0931ade/codex-rs/login/src/auth/revoke.rs).

OpenAI's current [OIDC discovery document](https://auth.openai.com/.well-known/openid-configuration) advertises authorization-code and refresh-token grants, S256 PKCE, public clients with token-endpoint authentication method `none`, RS256 ID tokens, revocation, and a JWKS URI. Its advertised `/api/accounts/...` endpoints differ from paths used by current Codex source. This reinforces that Morons must use an issued, documented contract rather than infer routes from another client.

Pi is useful non-normative comparison: it currently implements browser and device-code subscription login, five-minute refresh preflight, account-ID extraction, Codex backend headers, and OpenAI-hosted `web_search`. Morons will not import Pi's implementation, read Pi's token store, use Pi as an auth broker, or assume that Pi's working client identity grants Morons permission.

No reviewed OpenAI source found during this design establishes a general third-party native-client registration process or expressly authorizes an independent coding agent to reuse the Codex CLI client ID and private ChatGPT backend contract. Absence of a discovered document is not proof that no program exists, but it is insufficient authorization for Morons to ship the integration.

A second policy issue is data use. OpenAI documents that ChatGPT-authenticated Codex follows the selected ChatGPT workspace and account controls. Personal ChatGPT data may be used to improve models when the account setting permits it, while business offerings have different defaults and controls. Morons cannot infer a fixed no-training guarantee merely from possession of an access token or an unverified plan claim.

## Approval gates

No implementation may begin, and Brave Search remains the only shipped search adapter, until all of the following are recorded in this ADR or a superseding accepted ADR:

1. **OpenAI authorization:** OpenAI documentation or written authorization confirms that Morons may act as a native public OAuth client and invoke the intended ChatGPT subscription backend and hosted web-search capability.
2. **Dedicated client identity:** Morons receives or registers its own reviewed public client ID, exact loopback redirect URI set, and originator/application identity. Morons will not ship Codex's `app_EMoamEEZ73f0CkXaXp7hrann`, Pi's identity, or another application's client ID.
3. **Exact provider contract:** Authorization, token, JWKS, revocation, inference/search origins, paths, scopes, required headers, supported hosted-search models, rate limits, and response shapes are confirmed. Production routes remain hard-coded reviewed HTTPS values and cannot come from OIDC discovery, repository files, environment overrides, protocol input, model output, or remote catalogs.
4. **Data-use decision:** The owner explicitly approves adding a reviewed `ChatGPT workspace/account controlled` data-use classification, including the possibility of training under personal-account settings, or supplies a provider contract that establishes a stricter class Morons can verify.
5. **Product approval:** The owner approves this complete design, including the refresh uncertainty and logout behavior below.

Approval must identify the contract revision or dated correspondence being relied on. A successful experiment, another open-source client's behavior, or a token accepted by an endpoint is not approval.

## Proposed decision after approval

### Scope and provider identity

The first implementation supports **OpenAI ChatGPT subscription** as a distinct managed credential provider used only by OpenAI-hosted `web_search`. It does not automatically admit OpenAI as a root inference provider, add OpenAI models to `/model`, or add an OpenAI subagent setting. Those changes require separate model, wire-protocol, limit, context, data-use, billing, and live-qualification review.

OpenCode and OpenAI credential identities remain separate. `/login` becomes a typed provider chooser when more than one provider is supported. `/logout` lists configured providers when necessary and confirms one exact provider and observed identity generation. Existing OpenCode behavior and files remain backward compatible.

A new typed `/settings` row selects the web-search service and exact reviewed search model. Merely logging in does not express a model or billing preference. Migration retains Brave as the effective adapter until the owner deliberately selects an approved OpenAI pair. No catalog ordering, repository setting, prompt, skill, or model call can make that selection. The eventual removal of Brave requires a later release decision after OpenAI search is qualified.

### Login flow

Only the long-running authenticated server owns OAuth state and token exchange.

1. An authenticated local-owner request starts one bounded OpenAI login attempt. The server creates a random 256-bit attempt ID, random 256-bit OAuth state, random PKCE verifier, S256 challenge, and OIDC nonce. Secret-like values have redacted `Debug` implementations and never enter logs, SQLite, session history, commands, kernels, or model context.
2. The server binds one short-lived callback listener only to the exact approved loopback address and port. It never cancels, connects to, or replaces an unknown process occupying that port. Port conflict fails clearly.
3. The server returns a bounded authorization URL to the requesting authenticated client. The URL necessarily contains state and challenge and is treated as ephemeral sensitive interaction data: it is never durable, logged, included in errors, or returned after the attempt ends.
4. The client may open that exact URL with a reviewed no-shell platform browser launcher. If launch fails, it presents a terminal-safe bounded URL for deliberate manual opening. Browser launch is convenience, not token custody.
5. The callback listener accepts only bounded HTTP/1.1 `GET` requests for the exact callback path and approved `Host`, with bounded headers, target, query keys, connection count, and total deadline. State is checked in constant time and consumed once. Error responses are static escaped HTML and never reflect arbitrary query text.
6. The authorization code remains server-side. Morons does not accept codes, redirect URLs, access tokens, refresh tokens, or ID tokens through normal terminal text or IPC. Headless device login is omitted unless the approved OpenAI contract explicitly documents it for Morons.
7. The server dispatches one bounded authorization-code exchange with the retained PKCE verifier and exact redirect URI. It never retries after dispatch. A timeout, disconnect, malformed response, crash, or uncertain outcome ends the attempt; the user starts a new browser authorization rather than replaying a possibly consumed code.
8. Before installation, Morons verifies the ID token signature against keys from the fixed reviewed JWKS origin, issuer, audience, nonce, expiry, and required bounded account claim. JWKS responses and cache lifetime are bounded; discovery metadata cannot redirect requests. Access/refresh token and expiry fields are strictly bounded. Unknown account identity, implausible expiry, unsupported algorithm, missing rotated material, or validation failure rejects the login.
9. Only after full validation does the server perform the existing prepared/dispatched/installed credential mutation and publish sanitized configured status.

The attempt expires after at most ten minutes, supports exact cancellation, and is terminated on server shutdown or initiating-connection loss. It is ephemeral and never resumed after restart.

### Credential custody and representation

OpenAI OAuth material lives in a separately versioned `openai-chatgpt.state` beneath the existing dedicated credential root. It uses the same ordinary-file, ownership/DACL, link, bounded-size, synchronization, staged-write, atomic-replacement, and startup fail-closed controls as `opencode.state`.

The secret record contains only the minimum required access token, refresh token, validated account identifier, absolute expiry, identity generation, internal token revision, mutation marker, and refresh state. Morons does not retain email, profile, browser cookies, authorization codes, PKCE material, raw JWT claims, provider bodies, or an ID token after required validation unless the approved contract proves an ID token is needed for refresh or revocation.

Credential status exposes only provider kind, configured/reauthentication-required state, and a non-secret identity generation. It never exposes account ID, plan, token revision, expiry, token fragments, hashes, headers, or credential-derived identifiers.

Owner-only files do not protect tokens from arbitrary same-user processes. Morons still minimizes copies, wraps owned secret buffers in zeroizing types where review can establish it, and never claims forensic erasure or crash-dump protection.

### Identity generation and token refresh

Identity generation changes only when the owner logs in, replaces an account, logs out, or recovery completes such a deliberate identity mutation. A successful same-account token refresh preserves identity generation and increments an internal token revision. This prevents ordinary access-token rotation from invalidating work already bound to the same account while still making logout/account replacement observable.

Before an OpenAI request, the server acquires one provider-specific credential lease. If the access token has less than five minutes of validated lifetime, one process-wide refresh lock performs a double-checked preflight. Refresh uses the exact fixed token endpoint and one request only.

Refresh-token rotation is an uncertain external effect. The credential record therefore moves through source-bound `active → refresh_prepared → refresh_dispatched → active` states while retaining the old tokens:

- startup can safely clear `refresh_prepared`, because dispatch had not begun;
- startup, timeout, cancellation, transport loss, malformed response, or crash after `refresh_dispatched` changes the status to `reauthentication_required` and never retries the old refresh token;
- a valid response must preserve the validated account identity, provide a usable access token, and preserve or rotate the refresh token according to the approved contract before atomic installation; and
- explicit permanent failures such as expired, reused, revoked, or `invalid_grant` also require login.

A refresh failure never falls back to an environment variable, Pi/Codex state, OpenCode, Brave, another account, another model, or a stale access token. A 401 from a dispatched search may trigger refresh preparation for later work only if the contract safely permits it; the already dispatched search is never replayed automatically.

### Logout and revocation

`/logout` confirms the exact provider and last observed identity generation. For OpenAI OAuth, the server attempts at most one fixed-endpoint revocation of the refresh token (or access token only if the approved contract specifies that fallback) and never retries an uncertain revocation.

Local logout is not conditional on successful remote revocation. Once accepted, recovery must converge on removal of local OAuth material and incremented identity generation even if revocation fails, times out, or the server crashes after pessimistically marking dispatch. The UI reports one of `remote revocation confirmed`, `remote revocation not supported`, or `remote revocation uncertain; review the OpenAI account` without provider bodies or tokens. Local removal is not represented as forensic erasure and cannot retract an already dispatched request.

### Hosted web search

The approved adapter uses one exact reviewed OpenAI subscription service/model/protocol entry selected through `/settings`. Every search result records and displays that selection and its data-use classification. Unavailable or unauthorized selection fails without fallback.

Each call sends only the bounded search query and fixed server-owned search instructions to the fixed OpenAI hosted-search route. It does not send the parent transcript, selected directory, attachments, skill body, command output, environment, or arbitrary model-selected provider options. The request uses `store: false`, one hosted `web_search` tool, a required tool choice, fixed source inclusion, fixed output/token limits, and no client-supplied URL or headers.

The token and validated account identifier are scoped only to the exact approved origin/path. Redirects, ambient proxies, custom certificate roots, environment endpoint overrides, and remote route discovery are disabled. Search requests have independent connect, header, inactivity, and total deadlines and are never retried after dispatch.

The decoder bounds and strictly validates SSE framing, JSON depth and duplicate keys, event order, model text, citations, source count, URL/title/snippet lengths, usage, and terminal events. It accepts only reviewed hosted-search output variants. Raw requests, responses, headers, account identifiers, provider response IDs, reasoning, and error bodies are not persisted or logged. Canonical tool output contains a bounded answer, de-duplicated cited sources, selected search service/model/protocol, truncation state, and safe failure classification.

OpenAI and any upstream search providers receive the query and may apply external retention, residency, safety, and data-use policies. `store: false` is a request option, not a no-retention or no-training guarantee.

### Data-use presentation

If approval permits account-controlled ChatGPT data use, Morons adds a distinct manifest classification rather than labelling it `not used for training`. The UI must state, before first selection and in settings disclosure:

> Data use follows the selected ChatGPT workspace and account controls. Personal-account content may be used to improve models when that setting is enabled. Morons cannot verify the account setting.

A separate acknowledgement may record that the owner made this routing choice, but it is preference confirmation, not proof of provider policy. Business/Enterprise claims from a token do not silently upgrade the classification.

### Protocol, persistence, and audit boundaries

Implementation requires a protocol revision for typed provider-specific login attempts, sanitized credential statuses, exact cancellation, provider-specific logout, and web-search settings. Secret-bearing request/response types must have manually reviewed redacted `Debug` behavior.

SQLite may store only non-secret provider kind, mutation identity, generation, state transition, timestamp, idempotency, audit classification, and search service/model/protocol facts. It never stores OAuth URLs, state, nonce, verifier, authorization code, account ID, token revision, expiry, tokens, token hashes, JWT claims, request headers, or provider bodies. Credential files remain authoritative for secret state and refresh recovery; published status follows successful filesystem installation and any required SQLite reconciliation.

Login, token exchange, refresh, revocation, and hosted search are distinct external effects. None is automatically replayed after an uncertain dispatch. Sanitized errors use stable categories such as `login_cancelled`, `callback_invalid`, `token_exchange_uncertain`, `reauthentication_required`, `authentication_or_entitlement`, `rate_limited`, and `provider_unavailable`.

## Implementation sequence after approval

Each boundary remains a separate reviewed PR:

1. provider-neutral credential status/selector protocol and storage migration without network activity;
2. bounded server-owned OAuth login/callback/token validation with test-only injected origins;
3. serialized source-bound refresh and provider-specific `/logout` revocation/recovery;
4. reviewed OpenAI hosted-search manifest, explicit settings selection, bounded adapter, and Brave coexistence;
5. packaged clean-home and credentialed live qualification; and
6. only then, a separate decision on removing Brave.

No PR may combine a new credential origin with a new model inference origin merely for implementation convenience.

## Required validation

- entropy, PKCE, state, nonce, one-time callback, host/path/method, malformed query, request-count, port-conflict, timeout, cancellation, shutdown, and static HTML tests;
- exact authorization/token/JWKS/revocation URL and header scoping, redirect denial, TLS, bounded body, duplicate-key, JWT signature/issuer/audience/nonce/expiry/account, and no-retry tests;
- Unix ownership/mode/link/race/sync and Windows DACL/reparse/inheritance tests for every active, staged, removed, and refresh-state file;
- login, replacement, refresh rotation, concurrent refresh, account mismatch, permanent failure, uncertain refresh, crash recovery, logout, uncertain revocation, and generation tests;
- protocol, debug, log, audit, panic, subprocess-environment, command-argument, SQLite, backup, transcript, attachment, and kernel scans for complete and partial token fixtures;
- hosted-search request/SSE/source/citation/usage/limit/cancellation/uncertainty tests with one fixed private test injection boundary;
- six-target CI and packaging checks; and
- deliberate live login/search/logout without copying credentials to fixtures, commands, environment variables, screenshots, logs, or reports.

## Consequences

- The desired subscription-backed search has a complete proposed security and lifecycle shape, but no unsupported OAuth identity or backend route is shipped.
- Brave remains in place until the contract and product gates are approved and implementation is qualified.
- OpenAI OAuth tokens would receive the same dedicated server custody as OpenCode credentials while requiring stronger refresh and callback recovery state.
- Normal refresh can continue same-account work without changing owner identity generation; uncertain rotation fails closed and may require a new login.
- Search model choice and data-use disclosure become explicit owner preferences rather than consequences of credential access or catalog order.
- Supporting personal ChatGPT subscriptions requires an explicit product decision about account-controlled training policy.

## Alternatives rejected

- **Reuse Codex's or Pi's OAuth client ID or credential files:** misrepresents application identity, lacks reviewed authorization, and crosses credential-custody boundaries.
- **Treat a successful endpoint experiment as a provider contract:** cannot establish permission, stability, data use, or future compatibility.
- **Paste tokens or callback URLs into `/login`:** moves secret OAuth material into terminal/client protocol handling and weakens PKCE/state ownership.
- **Use browser cookies or automate ChatGPT pages:** scrapes an undocumented surface and exposes a much broader credential.
- **Automatically retry refresh, search, or inference after dispatch:** may consume a rotating token or duplicate billable work.
- **Fall back to Brave, OpenCode, an API key, or another model after OpenAI failure:** changes provider, credential, policy, or billing without owner intent.
- **Infer no-training status from `store: false`, plan text, or token claims:** none proves the effective workspace/account data controls.
- **Use an OpenAI Platform API key as if it were subscription auth:** the documented API route is technically simpler, but it uses separate usage-based billing and does not satisfy the selected ChatGPT-subscription goal; it remains a possible separate decision.
- **Replace Brave in the authentication PR:** combines credential, provider, search, migration, and release risks and removes the known fallback before qualification.
