# ADR 0004: OpenCode provider and credential boundary

## Status

Accepted

## Context

The trusted local server will invoke a model provider on behalf of durable agent runs. Provider requests can contain sensitive repository and conversation content, incur charges, and return malicious or malformed data. Provider credentials therefore belong to the server security boundary rather than the terminal client, session workspace, agent runtime, tools, kernels, or subprocesses.

The MVP needs one concrete provider integration. OpenCode documents direct API access to OpenCode Zen and OpenCode Go, a shared API-key credential convention, model-catalog endpoints, and model-specific inference endpoints. The two services have different billing and model availability even though they use the same OpenCode account credential.

Morons will support the models that OpenCode documents for its OpenAI Responses-compatible endpoints as a bounded permanent protocol surface.

Provider execution requires final local credential custody and mutation semantics. Environment inheritance is difficult to constrain once the server launches untrusted tools and subprocesses, and the existing security invariants exclude credentials from environments.

## Decision

### Supported provider surface

The MVP will support OpenCode Zen and OpenCode Go as distinct application-level services backed by one concrete OpenCode integration. Service identity remains part of every model selection and provider operation because the services have different routes, billing, limits, and model catalogs.

Production requests use only these documented HTTPS origins and paths:

- OpenCode Zen inference: `https://opencode.ai/zen/v1/responses`
- OpenCode Zen catalog: `https://opencode.ai/zen/v1/models`
- OpenCode Go inference: `https://opencode.ai/zen/go/v1/responses`
- OpenCode Go catalog: `https://opencode.ai/zen/go/v1/models`

The supported inference protocol is OpenAI Responses-compatible streaming. It is a permanent supported capability.

### Model admission and disclosure

A reviewed built-in manifest is authoritative for which service and model combinations Morons can invoke. Each entry binds a bounded exact model identifier to:

- OpenCode Zen or OpenCode Go;
- the Responses protocol revision supported by Morons;
- supported input, output, reasoning, and tool-call capabilities;
- server-enforced context and output limits; and
- a reviewed data-use and retention disclosure classification.

The remote catalog is untrusted availability input. It may suppress or mark a reviewed model unavailable, but it cannot add a model, change its service, select a wire protocol, grant a capability, alter a limit, or supply an inference URL. A catalog identifier not present in the built-in manifest is unsupported. Catalog fetch failure does not silently change the selected model or route.

Models documented as using prompts or completions for training, contributor programs, trials, or improvement are excluded from the built-in MVP manifest. Retention that does not involve training, including documented abuse-monitoring retention, is displayed as model metadata and is not represented as zero retention.

Model pricing, availability, retention, and provider policy can change independently of a Morons release. The provider's current terms remain authoritative. Morons does not silently substitute another model or switch between Zen and Go when a model is unavailable, a limit is reached, or an account lacks entitlement.

### Server and durable-state boundary

Provider invocation belongs to a concrete server-owned OpenCode module. It maps OpenCode wire data into provider-neutral run, message, tool-call, usage, and error domain types at the application and persistence boundaries.

Every provider operation durably identifies the selected OpenCode service, exact model identifier, supported protocol revision, normalized non-secret request options, context-construction policy version, and server-enforced limits. Request fingerprints exclude the API key and all credential-derived material.

The server commits accepted user input and the run identity before dispatching a provider request. Each billable provider request uses the prepared, dispatched, outcome, and uncertainty boundaries from ADR 0003. The server stores validated normalized outcomes, completed assistant messages, tool-call facts, bounded usage, and a bounded provider request identifier when useful for diagnosis. It does not persist raw request bodies, response bodies, headers, SSE records, or provider SDK objects.

Provider response identifiers and opaque continuation data are temporary run state, not session authority. Morons does not depend on provider-hosted conversation state for restart recovery and does not automatically resume an interrupted provider call. Context for a new run is constructed from canonical Morons history. Opaque data required only while continuing a live Responses tool loop remains bounded in trusted server memory and is discarded when the run terminates.

### Credential custody

One OpenCode API key may authorize Zen, Go, or both according to the user's OpenCode account. Morons stores one OpenCode credential and never infers account entitlement from possession of the key.

Persistent credentials reside in a dedicated server-owned credential root separate from IPC control state, SQLite data, backups, workspaces, configuration, and runtime directories. The credential state is bounded, strictly versioned, and atomically replaceable. It records only the OpenCode key and non-secret mutation metadata needed for generation checks and recovery.

On Unix, the credential root uses mode `0700` and every credential or temporary file uses mode `0600`; existing paths must be ordinary, owned by the effective user, and link-safe. On Windows, the root uses a verified protected DACL granting inheritable full control only to the current user and LocalSystem; credential files must be ordinary children that inherit that protection and are not reparse points. Required directory and file synchronization follows the same durability posture as other security-sensitive local state.

An absent credential is a valid unconfigured state and disables provider invocation. An existing malformed, insecure, unsupported, ambiguously replaced, or unreadable credential state fails closed and cannot be treated as unconfigured. Startup and mutation cleanup may remove only verified owner-controlled temporary files with the complete credential temporary-file grammar beneath the credential root.

The key is never stored in SQLite, database backups, audit facts, idempotency records, request fingerprints, configuration files, environment variables, command arguments, registrations, prompts, workspaces, sandbox files, logs, error strings, panic messages, or protocol responses. Confidentiality at rest relies on the owner-only operating-system controls and storage encryption already assumed by the local threat model.

Secret-bearing Rust and protocol types must use redacted `Debug` behavior. The client and server minimize secret copies and erase owned transient buffers where the implementation can provide a reviewed guarantee, without representing best-effort memory erasure as protection from the owning user, administrators, crash dumps, or a compromised process.

### Credential operations and recovery

The terminal client collects a key through non-echoing interactive input and sends it only after operating-system peer authorization, mutual local authentication, and protocol negotiation have completed. Credentials are never accepted from a command-line argument or environment variable.

This decision narrows ADR 0002's application boundary by permitting only the credential operations below over authenticated local IPC.

The server application boundary exposes narrowly scoped local-owner operations to configure, replace, remove, and inspect credential status. Status returns only whether a credential is configured and a non-secret state generation. No operation returns the key, a prefix, suffix, hash, fingerprint, or provider authorization header.

Credential replacement and removal use compare-and-swap semantics against the last observed state generation. The server writes a new versioned state to a fresh owner-only temporary file, synchronizes it, atomically replaces the active state, and synchronizes the credential directory. Removed state retains only the non-secret generation and recovery metadata needed to prevent an old update from being mistaken for current configuration. Filesystem and storage behavior do not provide a forensic-erasure guarantee.

A credential mutation cannot atomically commit with SQLite audit and idempotency records. The server therefore records non-secret prepared, dispatched, and outcome facts around the credential-file effect and includes a non-secret mutation marker in the credential state. Startup reconciles incomplete mutations from the active generation and marker before accepting credential or provider operations. Neither the database nor audit facts contain a secret verifier.

The client never automatically retries a credential-bearing mutation after a disconnect or unknown outcome. It queries credential status and generation, then asks the user to perform a deliberate new operation if the desired state is not established. Reuse of a completed or uncertain credential mutation identifier never applies newly supplied secret bytes.

Configuring a credential does not make a network request and does not claim that the key is valid or entitled to either service. Authentication and entitlement errors are reported only when a deliberate provider operation occurs.

### Outbound provider boundary

Only trusted server code can attach the OpenCode credential to an inference request. Production code constructs the origin and path from the selected service; protocol messages, repository content, model output, configuration files, and catalog responses cannot supply or override them.

The HTTP client requires HTTPS with normal certificate and hostname verification, disables redirects, and does not import proxy, certificate, credential, or endpoint configuration from repository files or process environments. The authorization header is attached only to the exact fixed inference origin and is never attached to catalog requests when the documented catalog is public.

Connection establishment, response headers, stream inactivity, and total provider execution have independent bounded deadlines. Requests have bounded serialized bodies, context, tool definitions, item counts, and output limits. Response headers, non-streaming error bodies, SSE records, decoded event fields, accumulated text, tool arguments, usage values, and provider identifiers have explicit limits.

Redirects, unexpected content types, malformed UTF-8, malformed JSON, unknown required event sequences, duplicate terminal events, events after termination, limit violations, and incomplete streams fail the provider operation. The server maps failures to deliberate sanitized application errors and never returns or logs raw provider bodies or headers.

Provider text, reasoning summaries, tool calls, tool arguments, citations, identifiers, usage, and errors are untrusted network input. Normalization does not make model output trusted. Tool names and arguments must still pass the separate server-owned capability and input validation boundary before execution.

### Retry, cancellation, and budgets

The MVP does not automatically retry an inference request after dispatch. A timeout, connection loss, malformed stream, cancellation race, or missing terminal event after dispatch may have consumed provider resources and remains a failed or uncertain external operation as defined by ADR 0003. Continuing the conversation creates a new run and provider operation rather than reusing an uncertain provider effect.

Cancellation aborts the controlled local HTTP task and durably records intent and the outcome that the server can prove. It does not claim that an upstream provider stopped computation or billing unless the provider contract establishes that fact. Client disconnection does not cancel a provider request.

The server enforces per-request context and output limits, per-run provider-call limits, total run deadlines, and global and per-session concurrency limits before dispatch. Provider account balances and subscription limits remain external controls. Monetary estimates based on mutable public pricing are advisory and never replace hard request, token, time, or concurrency limits.

### Implementation and validation

Production endpoint selection remains concrete and non-configurable. Tests may inject an in-process transport only through a private test boundary that cannot be selected by protocol data, configuration, repository content, or production command-line arguments.

Implementation requires:

- real cross-platform credential filesystem tests for ownership, modes, DACLs, links, replacement, synchronization, malformed state, stale temporary files, removal, and restart reconciliation;
- protocol tests proving authentication precedes secret transfer and secret-bearing messages and errors are redacted;
- deterministic Responses request and SSE fixture tests covering valid text, tool calls, usage, malformed order, duplicate termination, truncation, oversize fields, timeout, cancellation, and unknown events;
- HTTP boundary tests covering fixed routing, authorization-header scoping, disabled redirects, TLS and content-type failures, bounded error bodies, and absence of automatic retries;
- persistence tests covering prepared, dispatched, completed, failed, cancelled, interrupted, and uncertain provider operations without raw provider payloads or credentials;
- log and audit tests that search for complete and partial credential values; and
- an explicit local live test using a user-configured credential, never a credential committed to source, fixtures, CI, process arguments, or environments.

Any HTTP, TLS, secret-memory, or terminal-input dependencies must be pinned exactly, reviewed for license and transitive risk, and added only when the standard library and existing dependencies are insufficient.

## Consequences

- The MVP has two OpenCode service choices through one bounded Responses protocol surface.
- Responses support is a stable capability.
- A remote model catalog can only narrow the reviewed model manifest.
- Credential handling uses its final local custody boundary from the first implementation.
- Owner-only local storage works consistently on macOS, Linux, Windows, and headless systems.
- Provider and credential mutations add explicit filesystem, IPC, audit, recovery, and redaction work before the first live model request.
- Morons remains authoritative for sessions and durable history even though OpenCode and its upstream providers process deliberately sent context.
- Provider availability, account entitlements, policy changes, retention, upstream processing, and billing remain external dependencies and residual risks.
