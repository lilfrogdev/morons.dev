# Threat model

## Protected assets

- Provider credentials, account entitlements, and billable usage
- Local authentication key and endpoint registration
- Agent and session state
- Authoritative database, migration backups, and durable event history
- Tool and execution capabilities
- Repository, project, and isolated workspace data
- Provider and kernel connections

## Untrusted inputs

- IPC clients
- Authentication and application protocol messages
- Session, run, message, tool-call, mutation-request, model, pagination-cursor, and event-cursor inputs
- Endpoint, registration, database, backup, and workspace filesystem state
- Persisted payloads, schema versions, projections, and compaction checkpoints
- Repository content
- Model output
- Commands and subprocesses
- External content
- Provider model catalogs, HTTP headers, error bodies, SSE records, usage values, and identifiers

## Trust assumptions

- The operating system correctly enforces process identities, filesystem permissions, and Windows DACLs.
- Root, LocalSystem, administrators, and equivalent privileged accounts are outside the local IPC guarantee.
- Malicious processes already running as the owning operating-system user are outside the local IPC guarantee.
- Untrusted repository processes run without access to host control files, provider credential state, or IPC endpoints.
- OpenCode and its upstream providers receive only context deliberately selected for an authorized run; their catalogs, responses, errors, and model output remain untrusted.
- Public certificate authorities and the operating system's network and TLS implementations correctly authenticate the fixed OpenCode HTTPS origin.

## Local IPC threats

- A different local user connects to the legitimate server.
- A process impersonates the server or occupies a predictable endpoint before startup.
- A process replays a stale registration, endpoint, Host Epoch, process ID, nonce, or authentication proof.
- A crash between listener creation and registration publication leaves an unregistered Unix socket.
- Concurrent server startups race to publish different endpoints or delete a successor's state.
- A process tampers with or replaces the authentication key, host lock, control directory, or registration.
- A symbolic link, reparse point, or path race redirects creation, validation, publication, or cleanup.
- A connection is admitted before endpoint permissions or DACLs are installed and verified.
- A fake server causes the client to send credentials or application data before server authentication.
- A client sends malformed, oversized, partial, replayed, or stalled authentication records or application frames.
- A client disconnects during authentication or framing.
- A client uses a compatible protocol version without being authenticated or authorized.
- Authentication material is exposed through logs, prompts, environments, subprocesses, or sandbox files.
- An untrusted repository process discovers or reaches the host endpoint or authentication key.

## Application boundary threats

- A transport-authenticated client invokes an operation or accesses a resource it is not authorized to use.
- A client replays a mutation after a disconnect and duplicates an external side effect.
- A client supplies another session's resource identifier or event cursor to cross an authorization scope.
- A stale, malformed, or forged cursor causes events to be omitted, duplicated, or disclosed.
- A snapshot and event subscription race causes a client to miss committed state.
- Ephemeral progress is mistaken for authoritative state and cannot be recovered after a disconnect.
- A slow subscriber causes unbounded queue growth or blocks delivery to other clients.
- One session reads or modifies another session's workspace, runtime, history, or events.
- Concurrent input creates multiple nonterminal runs, bypasses global capacity, or enters an implicit queue.
- An input retry appends a second user message or creates a second run instead of returning the committed result.
- A client forges `LocalOwner` attribution, an assistant message, tool call, tool result, run transition, or terminal outcome.
- A stale cancellation request targets a successor run or records cancellation before controlled execution stops.
- New input proceeds while a tool or workspace effect remains uncertain, or an acknowledgement erases or resolves the uncertain fact.
- A client-local model choice, connection, or attachment becomes authoritative session state.
- Client detachment or disconnection unexpectedly cancels active work or transfers control of a session.
- A transport adapter bypasses server authorization, limits, idempotency, or audit enforcement.
- A protocol response exposes persistence fields, provider payloads, credentials, logs, or raw sandbox output.
- A prematurely exposed network listener admits unauthenticated, cross-origin, or unbounded requests.

## Provider and credential threats

- A credential is exposed through command arguments, environments, debug output, logs, errors, audit facts, request fingerprints, SQLite, backups, prompts, workspaces, subprocesses, or sandbox files.
- A fake client submits a credential before authenticating the server, or a credential-management response returns secret or credential-derived material.
- A missing, malformed, insecure, linked, stale, or partially replaced credential file is accepted as current configuration.
- A credential replacement races with another update, is retried after an unknown outcome, or loses its audit and recovery boundary across a crash.
- A run dispatches with a credential generation different from the generation accepted for that run.
- Repository input, configuration, model output, or a remote catalog selects an attacker-controlled origin, redirect, protocol, authorization scope, model capability, or provider route.
- A redirect, proxy, certificate override, or error-handling path forwards the authorization header away from the reviewed OpenCode inference origin.
- A malicious or compromised provider sends malformed, oversized, stalled, contradictory, or endless headers, errors, SSE records, JSON fields, tool arguments, identifiers, usage, or output.
- A remote catalog advertises an unreviewed model, protocol, capability, limit, training policy, or inference endpoint and is treated as trusted configuration.
- A model documented for training, contribution, trial logging, or improvement receives repository or conversation content without deliberate informed opt-in.
- A dispatched inference request is automatically retried after a timeout, disconnect, malformed stream, or cancellation race and duplicates billable usage.
- Provider response identifiers or opaque continuation data become authoritative conversation state and make restart recovery depend on provider retention.
- Raw provider requests, responses, headers, or streams enter canonical history or client DTOs and expose sensitive data or freeze an external wire format.
- Provider policy, pricing, retention, model availability, or upstream routing changes after a Morons release and invalidates local disclosure metadata or user expectations.

## Durable state threats

- A database, journal, backup, data directory, or workspace identity is replaced, linked, moved, opened from an unsafe filesystem, or given insecure access controls.
- A malformed or newer schema, corrupt canonical fact, or invalid persistent payload is accepted and interpreted as trusted state.
- A crash commits a projection, idempotency result, delivery event, or audit fact without its canonical fact, or publishes success before commit.
- A provider, tool, subprocess, or filesystem effect occurs but its outcome is not durably recorded before a crash.
- Startup recovery retries uncertain work, revives an old execution, or infers success from missing records.
- A cancellation is recorded as terminal while controlled execution may still be running.
- A partial assistant response or temporary progress update becomes authoritative history.
- Transcript entries lose actor, run, model, tool, or operation provenance or appear in a different order after projection repair.
- Transcript pagination and session-event attachment omit commits after the snapshot high water.
- A lossy compaction summary replaces or rewrites canonical history without validated source coverage.
- A migration partially applies, destroys the only usable state, silently downgrades, or recreates an unreadable database.
- Unbounded histories, tool results, event backlogs, idempotency records, or workspaces exhaust disk or memory.
- Workspace provisioning or deletion follows a forged path and modifies data outside the session workspace root.
- A live database file is copied inconsistently, a backup is disclosed, or a database-only backup is mistaken for complete workspace recovery.

## Mitigations

- Authorize the operating-system peer before reading connection bytes or issuing an authentication challenge.
- Use a persistent random 256-bit local key and a fresh random 128-bit Host Epoch for every server process.
- Require one server to hold an operating-system-backed host lock for its lifetime before changing control state.
- Keep a stable owner-only lock file that normal startup, cleanup, and shutdown never replace or remove.
- Use randomized per-epoch endpoints published through an atomic, owner-only registration.
- Use fresh 256-bit client and server nonces with role-separated HMAC-SHA256 proofs.
- Bind each proof to the authentication protocol version, Host Epoch, and both connection nonces.
- Verify proofs with a constant-time API and discard all connection authentication state after success or failure.
- Authenticate the connected server before sending application protocol messages or sensitive data.
- Accept provider credentials only through non-echoing client input after authenticated local IPC and never through command arguments or environments.
- Store provider credentials in a dedicated bounded and versioned owner-only root outside SQLite, backups, configuration, workspaces, runtime state, and IPC control state.
- Reject insecure, malformed, linked, unsupported, unreadable, or ambiguously replaced credential state and use expected generations, atomic publication, synchronization, and non-secret recovery markers for mutations.
- Expose only configure, replace, remove, and non-secret credential-status operations; never expose retrieval, fingerprints, raw storage, or a credential-bearing provider proxy.
- Build production OpenCode origins and paths exclusively from reviewed server code, disable redirects, retain certificate and hostname verification, and scope authorization headers to exact inference origins.
- Treat remote catalogs as availability input that can only narrow a reviewed built-in service, model, protocol, capability, limit, and data-use manifest.
- Exclude models documented for training, contribution, trial logging, or improvement from the reviewed model manifest.
- Bound and strictly decode provider requests, headers, error bodies, SSE records, decoded fields, accumulated output, tool arguments, usage, and identifiers under independent connection, header, inactivity, and total deadlines.
- Normalize provider outcomes into deliberate domain facts and sanitized DTOs without persisting or returning raw provider payloads.
- Record provider calls as prepared, dispatched, completed, failed, cancelled, interrupted, or uncertain external operations and never retry a dispatched inference request automatically.
- Record the credential generation accepted for each run, serialize credential mutations with dispatch checks, and reject a new dispatch when that generation changed.
- Keep provider response identifiers and opaque continuation data ephemeral to a live run and rebuild new-run context from canonical Morons history.
- Require an absolute, owner-controlled, non-group-writable, and non-world-writable Unix home directory before deriving control paths.
- Use owner-only Unix directories and sockets with bidirectional effective-user-ID verification.
- Use verified protected current-user-and-LocalSystem DACLs on Windows control, data, backup, and workspace directories before creating inheriting files.
- Install the protected owner-only named-pipe DACL atomically when the listener is created.
- Treat process IDs as advisory and require successful mutual proof.
- Reject insecure ownership, permissions, DACLs, links, malformed control files, unavailable peer identity, and inconsistent registrations.
- Publish registration only after listener security is verified and reject connections admitted before publication.
- Remove current-generation state only when its Host Epoch and endpoint still match the cleaning server.
- Remove registration-bound stale state only while holding the host lock and after validating owner control, registration shape, and endpoint confinement.
- Remove unregistered Unix sockets only when they are owner-owned and match the complete endpoint grammar inside the dedicated runtime directory.
- Apply independent size limits and non-resetting deadlines to authentication, framing, and application handshakes.
- Never treat protocol-version fields as authentication or authorization evidence.
- Keep authoritative security decisions in concrete server application services rather than transport adapters.
- Treat resource identifiers as opaque locators and authorize every operation against server-owned state.
- Attribute direct input to `LocalOwner`, atomically commit each accepted user message with one run, and prohibit clients from supplying assistant, tool, or run-outcome facts.
- Resolve exact input retries before concurrency checks, reject conflicting mutation reuse, and permit at most one nonterminal top-level run per session without a queue.
- Bind every run to an explicit reviewed service, model, protocol revision, credential generation, context-policy version, and server-owned limits.
- Reserve global execution capacity before input acceptance and retain it in a bounded server-owned run supervisor independent of client connections.
- Require cancellation to identify an exact session and run, commit intent before signaling the supervisor, and publish terminal cancellation only after controlled execution stops.
- Block new input after an uncertain tool or workspace effect and require an idempotent attributed acknowledgement of the exact blocker that preserves the uncertain facts.
- Give retriable mutations stable request identity and never retry uncertain external side effects blindly.
- Keep the authoritative database on a local owner-controlled filesystem and permit only the host-lock owner to access it through a bounded storage worker.
- Use a pinned bundled SQLite implementation with verified durable journaling, integrity, schema, foreign-key, defensive, extension, and resource-limit settings.
- Commit canonical facts, projections, idempotency outcomes, delivery events, and audit facts atomically, and publish durable results only after commit.
- Record prepared, dispatched, outcome, and recovery facts around external effects without holding a transaction across an effect.
- Terminate nonterminal runs from committed facts on startup, park uncertainty, and require a new run identity for continuation.
- Record cancellation as terminal only after controlled execution is known to have stopped; otherwise preserve an interrupted or uncertain outcome.
- Persist only complete bounded transcript entries with actor, run, model, tool, and operation provenance while keeping token deltas and temporary progress ephemeral.
- Page transcript snapshots at an immutable session-entry high water and return a session-event cursor from the same transaction for gap-free replay.
- Preserve canonical history through compaction and validate checkpoint coverage and source digests before replay.
- Run ordered transactional migrations, back up destructive migrations, and fail closed on newer, foreign, corrupt, or unsupported databases.
- Enforce database, payload, event, idempotency, and workspace quotas before accepting additional work.
- Confine workspace provisioning, recovery, and deletion to verified server-generated identities beneath the private workspace root.
- Use SQLite's online backup API, protect backup files like authoritative data, and distinguish database recovery from workspace recovery.
- Use scoped, server-validated durable cursors and gap-free snapshot-plus-subscription semantics.
- Keep token deltas, heartbeats, and temporary progress non-authoritative so their loss cannot corrupt recovered state.
- Bound every subscriber queue and disconnect slow consumers without blocking other clients.
- Isolate every session's mutable workspace and execution context, and authorize all cross-resource access explicitly.
- Serialize authoritative session mutations and enforce per-session and global execution limits.
- Make session lifetime independent of client attachments and require explicit authorized cancellation.
- Return deliberate sanitized protocol DTOs rather than persistence records or privileged subsystem payloads.
- Keep authenticated local IPC as the only current application transport and require a separate architecture decision before adding a network listener.
- Keep control files, provider credential state, and host IPC inaccessible from untrusted execution sandboxes.
- Exclude keys, nonces, proofs, and credentials from logs, audit payloads, prompts, environments, registrations, and endpoint names.

## Residual risks

- Root, LocalSystem, administrators, and equivalent privileged accounts can bypass local controls.
- Processes running as the same operating-system user are not distinguished from the legitimate CLI.
- Process separation alone does not sandbox untrusted commands.
- Compromise of the local authentication key permits impersonation until an explicit offline key replacement invalidates existing registrations and endpoints.
- Operating-system, filesystem, storage-device, SQLite, or cryptographic-randomness failures can invalidate durability and authentication assumptions.
- SQLite transactions cannot atomically commit external effects or mutable workspace files, so unresolved effects must remain interrupted or uncertain.
- Database backups do not recover workspace changes unless a separately bound workspace snapshot exists.
- Database confidentiality at rest depends on operating-system access controls and storage encryption rather than application-level encryption.
- Logical deletion and page reuse do not guarantee forensic erasure from filesystems, devices, or backups.
- Authorized same-user processes can read owner-controlled session and credential data, deny service, consume provider balances, or interfere with lifecycle state.
- Provider confidentiality depends on OpenCode, its upstream providers, their current policies, and deliberate context selection; submitted repository and conversation content leaves the local trust boundary.
- Provider availability, entitlement, pricing, retention, model behavior, and upstream routing can change independently of a Morons release.
- Ordinary certificate validation does not protect against compromise of the provider, a trusted certificate authority, the operating system, or the local server process.
- Owner-only credential files rely on operating-system access controls and storage encryption and do not provide forensic erasure or protection from the owning user, administrators, crash dumps, or a compromised server.
