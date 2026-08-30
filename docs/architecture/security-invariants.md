# Security invariants

## Local IPC trust boundary

- Protocol-version negotiation is compatibility checking, not authentication or authorization.
- Local transport authentication must complete before either process exchanges application protocol messages.
- The server must authorize the operating-system peer before reading bytes from it or disclosing authentication challenges.
- The client must authenticate the connected server before sending application messages, credentials, repository data, or capabilities.
- Authentication failures must close the connection without an application protocol response.
- The server remains authoritative for every privileged operation after connection authentication.
- Operating-system user authorization does not distinguish the CLI from another process running as the same user.

## Application service boundary

- Transport authentication admits a peer but never authorizes an application operation or resource.
- Every transport adapter must invoke the same server-owned authorization, capability, limit, idempotency, and audit enforcement.
- Resource identifiers are opaque locators and must never be treated as authorization evidence.
- Retriable mutations require stable request identity, and uncertain external side effects must never be retried blindly.
- Protocol responses and events must be deliberate sanitized DTOs rather than persistence records, provider payloads, logs, or raw sandbox output.
- Event subscriptions must be scoped to authorized resources, and resumable streams must use server-validated durable cursors.
- Snapshot and subscription semantics must not lose committed events between the snapshot position and stream attachment.
- Ephemeral events must never be required to reconstruct authoritative state and need not be replayed after disconnects.
- Per-subscriber queues must be bounded, and slow consumers must be disconnected rather than permitted unbounded memory growth.
- Authenticated local IPC is the only current application transport; a network listener requires a separate architecture decision and threat-model update.

## Provider credentials and model egress

- Only trusted server code may read provider credentials or attach them to an outbound request.
- Persistent provider credentials must reside in a dedicated owner-controlled credential root separate from IPC control state, SQLite data, backups, configuration, workspaces, and runtime directories.
- Provider credentials must never appear in command arguments, environments, SQLite, backups, request fingerprints, audit facts, registrations, model prompts, workspaces, sandbox files, protocol responses, errors, or logs.
- A missing credential is an unconfigured provider; an existing malformed, insecure, unsupported, unreadable, or ambiguously replaced credential state must fail closed rather than be treated as missing.
- Credential input may cross local IPC only after operating-system peer authorization, mutual authentication, and protocol negotiation complete, and secret-bearing types must redact debug output.
- Credential application services may configure, replace, remove, or report non-secret status and generation, but they must never return credential bytes or credential-derived fingerprints.
- Credential replacement and removal must use expected-generation checks, atomic owner-only publication, durable non-secret recovery markers, and no automatic retry after an unknown outcome.
- Production provider requests must use server-selected fixed HTTPS origins and paths; clients, repositories, configuration, model output, catalogs, and provider responses must not override an origin, protocol, credential scope, or inference route.
- Redirects must be disabled, certificate and hostname verification must remain enabled, and provider authorization headers must be scoped to the exact reviewed inference origin.
- A remote model catalog may reduce availability but must never enlarge the reviewed built-in service, model, protocol, capability, limit, or data-use manifest.
- The reviewed model manifest must exclude models documented for training, contributor programs, trials, or improvement.
- Provider requests, headers, response bodies, SSE records, decoded fields, accumulated output, tool arguments, identifiers, and errors must be bounded, strictly decoded, and sanitized before crossing the application boundary.
- Provider response identifiers and continuation data are ephemeral run state and must not become authoritative session or recovery state.
- A dispatched inference request must never be retried automatically because an uncertain outcome may already have incurred usage or billing.
- Provider failures and cancellations must preserve prepared, dispatched, outcome, and uncertainty facts without storing credentials or raw provider payloads.

## Session isolation and lifecycle

- One authoritative server may manage many sessions, but every operation and subscription must be authorized within its session scope.
- Session identity and durable lifetime must not depend on a server process, transport endpoint, or client connection.
- Client detachment or disconnection must not implicitly cancel an active run or transfer control of a session.
- Each session must have an isolated mutable workspace and execution context that cannot access another session's state without an explicit authorized capability.
- Temporary runtimes, subprocesses, and Python kernels must not become authoritative session storage or receive control-plane credentials.
- Session mutations and concurrent execution must obey server-enforced serialization, resource, concurrency, time, output, and budget limits.

## Durable state and recovery

- SQLite is the sole authoritative database for durable session, run, idempotency, event, compaction, and audit state.
- Only the server holding the lifetime host lock may open the authoritative database, and database access must pass through one bounded server-owned storage worker.
- The data, backup, credential, and workspace roots must be owner-controlled, local, link-safe, separate from local IPC control state, and inaccessible to untrusted execution.
- The authoritative connection must verify rollback journaling, `synchronous=EXTRA`, platform-supported full synchronization, foreign keys, untrusted schema handling, defensive mode, disabled extensions, and resource limits before serving operations.
- Durable payloads must be bounded, strictly decoded, and explicitly versioned independently of Rust layouts, SQLite rows, and protocol DTOs.
- Canonical facts and affected projections, idempotency outcomes, delivery events, and audit facts must commit atomically.
- A durable result or event must never be published before its transaction commits, and an unknown commit outcome must never be reported as success.
- External effects require durable prepared, dispatched, and outcome boundaries without holding a database transaction across the effect.
- A dispatched effect without a committed outcome is uncertain and must never be retried automatically.
- A run must not become cancelled until its controlled execution is known to have stopped; an unprovable cancellation remains interrupted or uncertain.
- Startup recovery must terminate nonterminal runs idempotently from committed facts before accepting application operations and must perform no external effect.
- Context compaction must preserve canonical history and bind every checkpoint to a validated ordered source prefix and digest.
- Schema migrations must be ordered and transactional, and newer, corrupt, foreign, or unsupported state must fail closed without automatic recreation or downgrade.
- Storage quotas must reject new work before uncontrolled growth and must never trigger silent deletion of canonical history.
- Live database backups must use SQLite's online backup API, receive owner-only controls, and never be represented as complete workspace backups.
- The database, backups, audit facts, request fingerprints, and workspace identity metadata must not contain server-managed credentials or local IPC authentication material.

## Authentication key and endpoint registration

- Each control root has a persistent, cryptographically random 256-bit local authentication key created with exclusive owner access.
- A missing, malformed, unexpectedly replaced, or insecure existing key must fail closed rather than be regenerated silently.
- Key replacement must be an explicit offline operation that invalidates every existing registration and endpoint.
- A key may be created automatically only while securely initializing a control root that did not already exist.
- Exactly one server may hold the control root's operating-system-backed host lock, and it must retain that lock for its lifetime.
- The stable lock file must never be replaced or removed during normal startup, cleanup, or shutdown.
- The key must never cross the IPC connection or appear in registrations, endpoint names, logs, audit events, prompts, environments, or sandbox files.
- Every server process has a new cryptographically random 128-bit Host Epoch.
- Endpoint names must be derived from the Host Epoch and must not contain authentication material.
- The control directory, key, and endpoint registration must be accessible only to the owning operating-system user.
- Existing control paths must be verified as owner-controlled and must not be followed through attacker-controlled symbolic links or reparse points.
- The endpoint registration must use a bounded, strict schema and bind the authentication protocol version, Host Epoch, endpoint, and server process ID.
- Registration publication must use an atomic same-directory rename, be performed only by the host-lock owner, and occur only after the listener's access controls have been installed and verified.
- The server process ID is advisory lifecycle information and is never sufficient authentication evidence.
- Normal shutdown may remove registration state only when it still matches that server's Host Epoch and endpoint.
- Registration-bound stale-state cleanup requires the exclusive host lock, verified owner control, a valid registration, and a constrained endpoint beneath the expected runtime root.
- Orphan cleanup without a registration may remove only owner-owned Unix sockets matching the complete endpoint grammar inside the dedicated runtime directory.

## Mutual proof

- Local authentication uses HMAC-SHA256 with fresh 256-bit client and server nonces.
- Client and server proofs must use distinct role tags and bind the authentication protocol version, Host Epoch, and both nonces.
- HMAC proofs must be verified with a constant-time verification API.
- A proof from one role, connection, Host Epoch, or authentication protocol version must not be valid in another context.
- Authentication records must be distinct from application messages and must never be interpreted as application protocol frames.

## Unix

- `HOME` must be absolute, owned by the effective user, and not writable by group or other users before it anchors control paths.
- Runtime, control, data, backup, credential, and workspace directories must be owned by the server's effective user and accessible only to that user.
- Database, journal, backup, credential, and workspace identity files must be ordinary owner-owned files and use mode `0600`.
- Socket files must be owned by the server's effective user, use mode `0600`, and reside beneath a mode `0700` owner-controlled directory.
- The server must verify that an accepted client's effective user ID equals its own before reading connection bytes.
- The client must verify that the connected server's effective user ID equals its own before beginning mutual proof.
- Missing peer credentials, unexpected ownership, or unavailable permission enforcement must fail closed.

## Windows

- Control, data, backup, credential, and workspace directories must use verified protected DACLs granting inheritable full control only to the current user and LocalSystem.
- Authentication keys, provider credentials, host locks, registrations, databases, journals, backups, and workspace identity files must be ordinary children of verified protected directories and inherit no access for untrusted principals.
- Named pipes must use `D:P(A;;GA;;;OW)`, installed when the listener is created.
- The connected server process ID must match the registered process ID when the platform provides it, but process IDs must not be the sole authentication boundary.
- Failure to construct, install, or verify required access controls must fail closed.

## Failure handling and isolation

- Connection, authentication, framing, and application handshakes must have bounded, non-resetting deadlines.
- Authentication records and application frames must have independent size limits and strict decoding.
- Connections admitted before endpoint security and registration publication are complete must be rejected.
- Authentication nonces and proofs must not be accepted more than once or retained after the connection attempt ends.
- Untrusted repository processes must not receive or be able to access the control directory, authentication key, provider credential root, endpoint registration, host IPC endpoint, data root, backup root, or another session's workspace.
- Authentication and authorization audit events must not contain keys, nonces, proofs, or other authentication material.
