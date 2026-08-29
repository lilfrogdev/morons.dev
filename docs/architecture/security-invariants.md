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

## Session isolation and lifecycle

- One authoritative server may manage many sessions, but every operation and subscription must be authorized within its session scope.
- Session identity and durable lifetime must not depend on a server process, transport endpoint, or client connection.
- Client detachment or disconnection must not implicitly cancel an active run or transfer control of a session.
- Each session must have an isolated mutable workspace and execution context that cannot access another session's state without an explicit authorized capability.
- Temporary runtimes, subprocesses, and Python kernels must not become authoritative session storage or receive control-plane credentials.
- Session mutations and concurrent execution must obey server-enforced serialization, resource, concurrency, time, output, and budget limits.

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
- Runtime and control directories must be owned by the server's effective user and accessible only to that user.
- Socket files must be owned by the server's effective user, use mode `0600`, and reside beneath a mode `0700` owner-controlled directory.
- The server must verify that an accepted client's effective user ID equals its own before reading connection bytes.
- The client must verify that the connected server's effective user ID equals its own before beginning mutual proof.
- Missing peer credentials, unexpected ownership, or unavailable permission enforcement must fail closed.

## Windows

- Control directories must use a verified protected DACL granting inheritable full control only to the current user and LocalSystem.
- Authentication keys, host locks, and registrations must be ordinary files created within the verified control directory and inherit no access for untrusted principals.
- Named pipes must use `D:P(A;;GA;;;OW)`, installed when the listener is created.
- The connected server process ID must match the registered process ID when the platform provides it, but process IDs must not be the sole authentication boundary.
- Failure to construct, install, or verify required access controls must fail closed.

## Failure handling and isolation

- Connection, authentication, framing, and application handshakes must have bounded, non-resetting deadlines.
- Authentication records and application frames must have independent size limits and strict decoding.
- Connections admitted before endpoint security and registration publication are complete must be rejected.
- Authentication nonces and proofs must not be accepted more than once or retained after the connection attempt ends.
- Untrusted repository processes must not receive or be able to access the control directory, authentication key, endpoint registration, or host IPC endpoint.
- Authentication and authorization audit events must not contain keys, nonces, proofs, or other authentication material.
