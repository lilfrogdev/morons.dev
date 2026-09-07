# ADR 0019: Native OpenAI ChatGPT subscription authentication

## Status

Accepted for staged implementation, revised 2026-09-06. Supersedes this draft's search-only scope and dedicated-client-registration gate. Browser sign-in and credentialed inference remain unqualified until deliberate owner testing. The OAuth core, custody and supervised terminal login are integrated. [ADR 0030](0030-native-provider-and-task-bindings.md) now connects reviewed native model selection and inference through provider-local run/task/compaction bindings and current data-use policy. Owner browser/subscription qualification remains separate.

## Context and provenance

The owner requested native Pi-like ChatGPT subscription authentication after completing the QA repairs and background compaction, and authorized implementation without importing another application's login. This is a separate credential and billing identity from OpenCode Zen/Go and an OpenAI Platform API key.

OpenAI's [Codex for OSS guidance](https://developers.openai.com/community/codex-for-oss), reviewed 2026-09-06, explicitly supports developers using Pi, OpenCode, Cline and other tools. [Authentication documentation](https://developers.openai.com/codex/auth) distinguishes ChatGPT subscription access from API-key billing and describes account/workspace-controlled data handling. These sources do not establish a Morons-specific support agreement or guarantee endpoint stability. They also do not establish that a separately registered client is a technical prerequisite.

Implementation references are Pi 0.84.2 (`@earendil-works/pi-ai`, `auth/oauth/openai-codex.js`) and OMP commit [`0fcdbb30`](https://github.com/can1357/oh-my-pi/blob/0fcdbb30f6b532365d9b7ec7e38c86ae2fae91ec/packages/ai/src/registry/oauth/openai-codex.ts). They use the same native public client identity, authorization-code/S256 flow and token endpoint. References establish interoperability provenance, not a dependency, credential source or full security audit. Morons independently implements the bounded Rust flow and identifies itself as Morons, not Pi or Codex.

The prior draft combined authentication with hosted search, required a dedicated client ID and mandatory OIDC/JWKS verification, and excluded ordinary coding inference. Those were draft design choices, not proven provider requirements. This revision explicitly changes them rather than silently bypassing a recorded boundary.

## Scope and fixed contract

The intended integration is **OpenAI ChatGPT subscription coding inference**, including root runs, compaction and explicitly selected children. No automatic model selection follows login. Brave remains the only shipped `web_search` adapter; hosted search, device login, API-key login, browser-cookie access and other-app credential import are outside this increment.

OAuth compatibility revision 1 fixes:

| Purpose | Value |
| --- | --- |
| Authorization | `https://auth.openai.com/oauth/authorize` |
| Token exchange and refresh | `https://auth.openai.com/oauth/token` |
| Public client ID | `app_EMoamEEZ73f0CkXaXp7hrann` |
| Redirect | `http://localhost:1455/auth/callback` |
| Listener | `127.0.0.1:1455`, no wildcard or alternate port |
| Scope | `openid profile email offline_access` |
| Application originator | `morons` |
| Extra authorization fields | `id_token_add_organizations=true`, `codex_cli_simplified_flow=true` |
| PKCE | S256, 32 random verifier bytes encoded base64url without padding |
| State | Independent 32 random bytes encoded base64url without padding |

The public client ID is not a secret or an imported credential. This deliberately follows the reviewed native-client compatibility flow without claiming a private Morons registration. If the provider rejects Morons' identity or requires registration, report the blocker; do not silently impersonate another originator, change client IDs, fall back to an API key or copy another application's login.

[ADR 0028](0028-native-codex-responses-contract.md) records the staged full-Responses adapter review, including model retirement and local-only output limits. Application admission follows the completed bindings in [ADR 0030](0030-native-provider-and-task-bindings.md). The coding adapter separately reviews `https://chatgpt.com/backend-api/codex/responses`, `store: false`, account-scoped headers, exact supported models, tools/images, output limits, reasoning continuation and strict Responses decoding. The token endpoint cannot select or redirect to this or any other origin. No inference route or model is enabled by the OAuth-core increment.

## OAuth core

Only trusted server code initiates a login. Application integration must first authenticate the local owner, enforce one global nonqueued attempt, capture the observed OpenAI identity generation, and own the task until cancellation/shutdown has drained. IPC may initiate the supervised core after local authentication; tools cannot initiate it.

- Bind the fixed loopback listener before returning an authorization URL. Port conflict is an error; never kill, connect to, replace or discover credentials from the port owner.
- A login object owns its verifier, state, listener and ten-minute monotonic deadline. Its consuming completion future must be promptly polled under application supervision; an unpolled Rust object does not run a timer. Completion permits at most one code exchange. Drop closes its listener. Cancellation wins before polling exchange work or returning tokens; no automatic retry.
- The URL is deliberately exposed only for browser interaction. It contains state and a challenge, not the verifier, code or tokens. Redact it in `Debug`; never persist it, put it in model input or general status/log messages. The future client displays it in a dedicated terminal-safe ephemeral dialog. No shell-based launcher, callback paste or code entry is admitted here.
- Callback HTTP is a small fixed grammar, not an application server: HTTP/1.1 GET, exact `/auth/callback`, exact `Host: localhost:1455`, no body/transfer encoding, no origin-form ambiguity, duplicate headers or duplicate query fields. Bound request bytes to 8 KiB, headers to 32, each connection to five seconds and the whole attempt to ten minutes. Accept at most 16 connections sequentially; mismatched/invalid requests cannot consume a valid code or renew the deadline. Responses are static, have no reflected query values, disable caching and have a restrictive CSP. Receiving a code is not reported as completed credential installation.
- Compare decoded state in constant time. Accept one bounded visible-ASCII code and matching state, or a bounded provider error with matching state. Unknown query fields, malformed escapes, duplicate/contradictory results and controls reject. No code, verifier, token, query target or raw error is logged.
- The exchange uses one form-encoded POST with fixed grant, client and redirect. TLS uses the existing reviewed web-PKI Hyper stack; no redirects, ambient proxy/certificate override, generic URL input, cookies, automatic retry or SDK discovery.
- The exchange has a 30-second total limit, ten-second connect/header/inactivity limits, 32 headers/8 KiB header bound and 64 KiB body bound. Header-parser allocation is bounded as well as validated decoded headers. Reuse the existing HTTPS constructor without changing OpenCode's parser settings or error classifications. Non-200, malformed, oversized, timed-out or interrupted replies fail without replay. Uncertain exchange means start a new owner-authorized login, not reuse the code.
- Token responses have closed duplicate-rejecting fields: access/refresh tokens, integer `expires_in`, and optional reviewed `token_type`, `scope`, `id_token`. Each secret is at most 16 KiB of visible ASCII. A present token type must be Bearer; a returned scope must equal the requested set without additions or duplicates. Lifetime must exceed the five-minute refresh margin and be at most 30 days. Discard an optional ID token after bounded decoding of the envelope; do not store email/profile/plan data.

### Token trust, account routing and memory

This is OAuth resource access, not a general OIDC identity verifier. Tokens are accepted only from the fixed authenticated TLS exchange of the retained code/verifier. There is no IPC/token-string import API. No JWKS fetch, ID-token signature claim, OIDC nonce or ID-token identity assertion is added merely to imitate a draft. This is an explicit change from the original proposal.

The access token's bounded JWT payload is decoded solely for the required `https://api.openai.com/auth.chatgpt_account_id` routing value and expiration. JSON duplicate keys reject at every level. The account is an opaque bounded header value, not proof of local authority, entitlement, training policy or workspace settings. Use the earlier of token-response and JWT expiration; reject expired, missing or malformed required claims. A refresh must retain the exact account. Signature verification is left to the resource server; Morons does not claim it verified a JWT cryptographically.

Secret-bearing types have redacted `Debug`, no general serialization/clone API and zeroizing owned buffers, including form bodies retained by the HTTP request. The decoder minimizes retained strings and clears the successfully parsed claim tree after extraction or validation failure. Parser, transport and allocator copies may remain; this is not protection against crash dumps or same-user processes. See [security invariants](../architecture/security-invariants.md).

## Credential persistence and lifecycle integration

A distinct versioned `openai-chatgpt.state` uses the existing bounded storage worker and private-file/atomic-replacement controls. Extend the credential-directory validator with only the exact new active/temporary grammars. OpenCode file bytes and generations remain unchanged. SQLite 28 rebuilds the existing credential mutation request table with an explicit credential kind (old rows become OpenCode), scopes generation uniqueness to that kind, and reuses existing request/fact/audit transactions and the combined 10,000-mutation bound. No OAuth secret or token revision enters those tables.

Store only required access/refresh tokens, opaque account, expiry, identity generation, token revision and non-secret recovery marker/state. Never store these secrets or account/expiry/revision in SQLite, backups, protocol status, prompts, tool arguments, environments or logs. Credential status reports only provider, configured/reauthentication-required and identity generation. Configure/remove operations need existing durable non-secret prepared/dispatched/outcome evidence and reconciliation; URL/code exchange remains ephemeral and non-replayable.

The bounded binary credential file records Removed, Active, RefreshDispatched or ReauthenticationRequired; identity generation and last owner-mutation marker are separate from token revision and refresh-attempt marker. A source-bound checksum detects corruption, not malicious same-user rewriting. A successful same-account refresh increments revision, not identity generation. The shared credential directory accepts at most 128 entries. Writes use fresh exact-grammar temporary names, file sync, atomic replacement, directory sync and readback; any uncertain write poisons further use until restart reconciliation.

Serialize OpenAI dispatch/mutation with one provider-specific lease, independent of OpenCode. An expiring access token causes a durable RefreshDispatched write before a one-use refresh grant leaves the worker. Refresh uses the fixed token endpoint and existing bounded decoder, requires a refresh token in the reply, and pins the account. Success installs the next revision before returning an access-only dispatch lease. Failure or abandoned ownership requires reauthentication without replay; old material remains in the disabled private state until deliberate replacement/logout. There is no separate prepared refresh file: a crash even just before network transmission may conservatively require login. Status queries do not recover a live dispatch. Startup, or a later caller holding the exclusive lease after prior ownership ended, may terminalize abandoned RefreshDispatched state. A 60-second lease-resolution deadline includes lock/worker waits and the 30-second exchange, with at most five additional seconds for cancellation/timeout persistence cleanup; cancellation never detaches a refresh task. Do not replay the inference that returned 401.

Login/account replacement/logout change identity generation, invalidating later stale dispatches. Logout is deliberate provider-specific local removal with explicit disclosure that remote authorization may remain; no unreviewed revocation endpoint is contacted and no remote-revocation guarantee is made. Local removal is not forensic erasure and cannot retract dispatched work. Existing configured credentials remain intact if a login is cancelled or fails before installation.

### Authenticated terminal control

IPC 40 adds provider-specific credential status/removal and a dedicated login connection. Begin captures an exact observed OpenAI generation and an owner mutation identity; it does not accept codes, tokens, callback URLs or endpoints. One nonqueued supervisor slot owns the login task through completion, with cancellation on owning-connection loss, explicit exact-attempt cancellation or server shutdown. Only the initiating connection receives the redacted, bounded browser URL. Other connections cannot inspect or cancel its attempt by supplying an identifier. Task joining uses server-owned registration identity, not equality of client mutation identifiers; a reused identifier cannot attach an old connection to a new login. Login traffic is not a session event or canonical transcript entry.

The dedicated connection receives a start response and one terminal login result. It is not multiplexed with ordinary requests or subscriptions. All writes are bounded; a slow/invalid/disconnected consumer loses its attempt, not a session or root run. The server retains the task and joins it before slot reuse or shutdown; a dropped connection handler signals cancellation synchronously rather than spawning detached cleanup. OAuth completion remains bounded to ten minutes. Installation has a separate 60-second wait bound: cancellation before installation preserves the old credential, but cancellation after installation begins waits for its outcome and can report Installed. A lost/timed-out storage acknowledgement is InstallationUncertain, never a claim of rollback, and requests fail-closed server shutdown; uncertain worker effects may still commit and are reconciled on restart, not retried.

The client uses a separate owned, non-reconnecting authentication connection with bounded event delivery. `/login` and `/logout` choose OpenCode or ChatGPT explicitly; existing non-echoing OpenCode input remains. ChatGPT shows provider status and asks for confirmation before login/replacement or local removal. The URL is terminal-safe, ephemeral and manually opened in the owner's browser; there is no shell launcher, clipboard import, callback paste or automatic login/model selection. Esc clears the URL and requests cancellation; a disconnect/lost result instructs status reload rather than replay. Local logout warns that remote authorization and previously dispatched work may remain. Only synthetic isolated fixtures are used before the owner's deliberate live qualification.

## Coding integration and data use

Do not disguise OpenAI as a third OpenCode billing service. Introduce explicit provider/credential identity at model selection, run acceptance, prepared dispatch, children and maintenance boundaries; preserve historical OpenCode encodings and validation. A cross-provider child requires its own exact credential generation bound durably to the outer task before dispatch, not reuse of an unrelated parent's generation. Maintenance inherits its trigger's exact provider/account generation and cannot reroute.

Reviewed manifests, not token claims/catalogs, define models, protocol revisions, image/tool limits and account-controlled data-use disclosure. Neither `store: false` nor a business-plan claim proves no training or zero retention. Preserve the owner's earlier product choice: independently optional Block training use and Require zero data retention restrictions, both default off. Account-controlled/unverifiable entries cannot pass either restrictive policy by assuming a favorable account setting. Implement consistent admission across every enabled selection/dispatch surface before adding OpenAI models.

## Stages and qualification

1. OAuth core, private loopback tests, redaction/bounds/cancellation/one-exchange tests; no user login yet.
2. Provider-specific custody, durable mutation/refresh recovery, authenticated login/status/cancel/logout and ephemeral terminal interaction. No real credentials in tests/CI, no retained QA migration without an approved plan.
3. Exact reviewed coding model/Responses integration and policy controls across root/child/compaction. No generic endpoint/provider plugin mechanism or silent fallback.
4. Owner browser sign-in plus a separately approved small request budget; test text/tools/images, refresh/account change/cancellation and restart. Preserve failures and keep reports outside Git. Only then claim live readiness.

Run formatting, all-target checks, warnings-denied Clippy, dependency policy and deterministic tests for every increment. Expand loopback fixtures for hostile callbacks, port conflicts, slow/incomplete connections, cancellation before/after code receipt, wrong state, duplicate fields, token/claim bounds, content types, redirects, fixed route/form fields, lifetime, account mismatch, and no automatic retry. Persistence integration additionally needs native ownership/DACL, torn replacement, crash/recovery and secret-exclusion coverage. Platform CI, sampled live behavior and exact native/release artifacts remain distinct gates.
