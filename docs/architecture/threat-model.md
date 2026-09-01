# Threat model

## Protected assets

- Provider credentials, account entitlements, and billable usage
- Local authentication key and endpoint registration
- Agent and session state
- Authoritative database, migration backups, and durable event history
- Tool and execution capabilities
- Original repository, immutable workspace baseline, active worktree generation, and nonauthoritative command candidates
- Sandbox execution images, private caches, process-tree ownership, and confinement policy
- Provider and kernel connections
- Terminal presentation integrity and non-echoing credential input
- Packaged client, server, and sandbox-helper executable identity

## Untrusted inputs

- IPC clients
- Authentication and application protocol messages
- Session, run, message, tool-call, mutation-request, model, pagination-cursor, and event-cursor inputs
- Endpoint, registration, database, backup, and workspace filesystem state
- Persisted payloads, schema versions, projections, and compaction checkpoints
- Repository source paths, names, metadata, links, reparse points, special files, content, and concurrent changes
- Model-selected tool names, call identifiers, worktree-relative paths, arguments, replacement text, and tool-result content
- Model-selected command programs, arguments, working directories, descendants, output, exit state, generated files, and resource use
- Mutable worktree entries, links, reparse points, identity changes, stale digests, command candidates, generation state, operation staging state, and concurrent changes
- Model output
- Commands and subprocesses
- External content
- Terminal key, paste, resize, mouse, and rendering input
- Companion executable paths, process state, inherited environments, and startup races
- Provider model catalogs, HTTP headers, error bodies, SSE records, usage values, and identifiers

## Trust assumptions

- The operating system correctly enforces process identities, filesystem permissions, Windows DACLs, namespaces, Landlock, seccomp, Seatbelt, AppContainer capabilities, and process-tree termination primitives that the selected native backend verifies.
- Root, LocalSystem, administrators, and equivalent privileged accounts are outside the local IPC guarantee.
- Malicious processes already running as the owning operating-system user are outside the local IPC guarantee.
- Untrusted repository processes run without access to host control files, provider credential state, IPC endpoints, workspace baselines or metadata, original source trees, or other sessions' worktrees.
- Same-user processes that independently modify a selected source repository during import remain outside the local isolation guarantee; Morons still rejects observed entry-type and file-identity changes.
- OpenCode and its upstream providers receive only context deliberately selected for an authorized run; their catalogs, responses, errors, and model output remain untrusted.
- Public certificate authorities and the operating system's network and TLS implementations correctly authenticate the fixed OpenCode HTTPS origin.
- Supported native runners and release hardware accurately execute the declared operating-system and processor target.

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
- A fake or replaced companion executable is selected through `PATH`, configuration, repository state, or a writable installation-relative path.
- A client treats a spawned process identifier, status, or readiness output as proof that it reached the legitimate server.
- Concurrent clients race to start servers and a losing process deletes or replaces the winner's registration.
- Repository, provider, proxy, certificate, dynamic-loader, or credential environment state is inherited by an automatically started trusted server.

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
- A model requests an undeclared tool, malformed or duplicate calls, contradictory final output, an absolute or escaping path, cross-session state, or excessive calls and receives an effect before the complete provider response is durably validated.
- A client submits an arbitrary command, environment, host path, standard input, mount, network option, or sandbox policy, or a model-selected command bypasses the bound execution image and active run.
- Tool output from one session, run, call, or workspace is attached to another scope or supplied to a provider before its result commits.
- A stale cancellation request targets a successor run or records cancellation before controlled execution stops.
- New input proceeds while a tool or workspace effect remains uncertain, or an acknowledgement erases or resolves the uncertain fact.
- A client-local model choice, connection, or attachment becomes authoritative session state.
- Client detachment or disconnection unexpectedly cancels active work or transfers control of a session.
- A transport adapter bypasses server authorization, limits, idempotency, or audit enforcement.
- A protocol response exposes persistence fields, provider payloads, credentials, logs, or raw sandbox output.
- A prematurely exposed network listener admits unauthenticated, cross-origin, or unbounded requests.
- User, provider, catalog, error, or tool text injects terminal control sequences, hyperlinks, title changes, clipboard operations, device commands, or misleading bidirectional layout.
- Client reconnect creates a new mutation identity after an unknown result and duplicates input, cancellation, session creation, or shutdown intent.
- A credential-bearing mutation is resent after an unknown result or remains in terminal history, rendered cells, configuration, panic output, or client memory longer than required.
- Terminal exit implicitly cancels a run or stops the server, or a client kills an unrelated process based only on a registered process identifier.
- Ephemeral assistant deltas are displayed under the wrong session or run, accepted out of sequence, or mistaken for canonical transcript state.
- A client imports a repository into a non-pristine, active, blocked, or already imported session or races another import.
- A repository source or destination path becomes authorization evidence, durable identity, model context, audit data, or exposed client state.

## Provider and credential threats

- A credential is exposed through command arguments, environments, debug output, logs, errors, audit facts, request fingerprints, SQLite, backups, prompts, workspaces, subprocesses, or sandbox files.
- A fake client submits a credential before authenticating the server, or a credential-management response returns secret or credential-derived material.
- A missing, malformed, insecure, linked, stale, or partially replaced credential file is accepted as current configuration.
- A credential replacement races with another update, is retried after an unknown outcome, or loses its audit and recovery boundary across a crash.
- A run dispatches with a credential generation different from the generation accepted for that run.
- Repository input, configuration, model output, or a remote catalog selects an attacker-controlled origin, redirect, protocol, authorization scope, model capability, provider route, tool definition, or tool capability.
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
- Repository import follows a symbolic link, junction, reparse point, special entry, changed file identity, `..` component, case collision, or normalization collision and reads or writes outside the intended tree.
- A selected source overlaps a Morons private root and copies provider credentials, authentication material, databases, backups, baselines, or another session's worktree into the imported repository.
- Git control data enters the workspace and later influences trusted Git behavior through hooks, configuration, credential helpers, worktree indirection, remotes, or alternates.
- Import writes to the original repository, exposes it to untrusted execution, or lets a mutable worktree alter its immutable baseline.
- File count, depth, path length, individual size, total size, sparse expansion, staging state, or manifest construction exhausts memory, disk, or server capacity.
- A crash leaves incomplete or ambiguously published import state that is mistaken for a complete repository or causes the source to be reread automatically.
- Baseline and worktree bytes diverge during import, making later diff review depend on an invalid comparison point.
- A file tool follows a link, reparse point, alternate stream, special file, changed parent, case alias, normalization alias, or traversal component and reaches the baseline, workspace metadata, host filesystem, or another session.
- A stale digest or ambiguous replacement silently overwrites worktree content different from what the model observed.
- A create operation replaces an existing entry, implicitly creates attacker-selected parents, or publishes a temporary file with insecure controls.
- A crash after tool dispatch but before result commit causes an edit to be repeated, an unapplied staged edit to be published, a successful mutation to be forgotten, or an unmatched call to enter later provider context.
- A command runs against the authoritative worktree, an interrupted candidate becomes active, a generation pointer commits without its command result, or stale generation cleanup removes the current worktree.
- A command candidate creates links, reparse points, alternate streams, special files, invalid names, collisions, oversized artifacts, hostile controls, or auxiliary metadata that trusted promotion accepts.
- A read or search result is unbounded, recreated from changed bytes after restart, contains binary or terminal-control content treated as trusted, or exhausts memory, context, storage, or provider limits.
- A mutating tool outcome remains ambiguous but the run is marked failed or cancelled without an uncertainty blocker and a successor run modifies the same workspace.
- A live database file is copied inconsistently, a backup is disclosed, or a database-only backup is mistaken for complete workspace recovery.

## Sandboxed command threats

- A fake or replaced sandbox helper is selected through `PATH`, repository content, configuration, a writable installation path, or a forged one-shot launch specification.
- A command reads the baseline, workspace metadata, original source, another session, owner home, provider credentials, IPC authentication material, databases, backups, shell history, package credentials, or host runtime state outside its explicit image.
- A command writes outside its candidate, alters a shared toolchain or cache, reaches the authoritative worktree before publication, or poisons another session's execution environment.
- A process connects to the Internet, loopback, a Unix socket, a Windows named pipe, the Morons endpoint, a package registry, a metadata service, or another host-local service.
- A descendant escapes its sandbox, process group, PID namespace, AppContainer, or Job Object; signals or inspects the server; creates a new namespace; retains an inherited handle; daemonizes; or survives cancellation and server loss.
- A Windows process begins before Job assignment, requests breakaway, inherits a host standard or control handle, or leaves an operation SID on a shared image, installation, or trusted root.
- A native Windows FFI bug misstates pointer, buffer, handle, ownership, thread, or lifetime requirements and weakens AppContainer, ACL, process-attribute, or Job enforcement.
- Repository configuration redirects Cargo, a compiler, linker, runner, shell, credential provider, or package manager to a host path or executable outside the execution image.
- Host environment variables, dynamic-loader settings, proxy state, credential helpers, agents, terminal handles, current directories, or absolute paths enter the sandbox.
- Output blocks one pipe, exhausts memory or disk, emits invalid bytes or terminal controls, discloses a host path, or reaches the model or terminal before becoming a committed bounded result.
- A partially enforced or unavailable platform sandbox silently falls back to ordinary host execution.
- A malicious toolchain, dependency, build script, procedural macro, compiler, test, or generated executable exploits the operating system or sandbox backend.

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
- Bind every run to an explicit reviewed service, model, protocol revision, credential generation, context-policy version, tool-catalog version, and server-owned limits.
- Offer only fixed concrete server-owned tools supported by the reviewed model, strictly decode a complete provider response into typed calls, and commit every call before dispatching the first one.
- Execute calls sequentially under one session workspace lease, commit each typed bounded result before another provider turn, and never accept authoritative calls or results from an IPC client.
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
- Permit repository import only for the authenticated local owner and only once into a pristine session with no transcript, run, blocker, or repository state.
- Treat the submitted source path as bounded transient input, never persist or return it, and bind exact retries through a normalized operation fingerprint that does not contain the path bytes.
- Traverse only ordinary bounded UTF-8 directories and regular files beneath the validated source root; reject links, reparse points, special entries, identity changes, escapes, and destination collisions.
- Reject source roots that equal, contain, or are contained by Morons application, control, runtime, data, backup, credential, or workspace roots.
- Omit every `.git` component and subtree, copy no source ownership or control metadata, and never invoke Git, hooks, repository code, provider inference, or source writes during import.
- Build an immutable baseline and separate mutable worktree from the same file bytes under one operation-specific staging directory, bind them with a canonical manifest digest, and publish their parent atomically.
- Give untrusted execution only an operation-specific candidate copied from the active worktree and keep the authoritative generation, baseline, metadata, workspace root, source repository, control state, persistence, and credentials outside its capability boundary.
- Resolve structured tool paths from a pinned worktree root through handle-relative no-follow traversal, reject alternate streams and special entries, and perform mutations relative to a pinned destination parent.
- Bound and verify UTF-8 file reads and literal searches, return complete-file SHA-256 digests, and expose only repository-relative names and typed truncation state.
- Require digest-matched unambiguous edits, exclusive creates, private operation staging, synchronization, and atomic replace-or-no-replace publication without source or baseline writes.
- Use prepared, dispatched, completion, idempotency, audit, and recovery facts around import; never reread the source after uncertain dispatch and reconcile only exact confined operation-bound staging or published state.
- Record prepared, dispatched, and terminal facts for every tool call; never rerun or resume one on startup, and reconcile a mutation only from exact target identity, operation staging state, and committed before-or-after digests.
- Resolve commands only through one bound immutable execution-image generation, a fixed empty environment, structured arguments, a relative candidate working directory, closed standard input, bounded pipes, and an exact packaged one-shot sandbox helper selected without `PATH`, shell, repository, configuration, or model input.
- Use verified Linux namespaces with Landlock and seccomp, a default-deny macOS Seatbelt profile, or an operation-specific Windows AppContainer and non-breakaway Job Object; fail closed when complete enforcement is unavailable.
- Confine Windows native FFI to one target-only internal adapter with no public binary or generic launch API, keep unsafe code denied elsewhere, and require documented preconditions for every unsafe block.
- Create Windows bootstrap processes suspended with an exact capability and handle list, assign the configured Job before resume, use only dedicated standard/control handles, and verify zero active Job members before accepting a result.
- Apply AppContainer ACLs only to operation-private candidate, cache, runner, and image views so profile deletion and exact staging cleanup remove every operation grant without mutating shared trusted state.
- Deny all command network and host-local service access, inherited terminals and handles, host process inspection and signaling, namespace weakening, privilege changes, and background descendants.
- Execute commands only against nonauthoritative candidates, copy admissible normal-exit output into a clean synchronized generation, and atomically commit its active pointer with the terminal command result.
- Discard candidates after cancellation, timeout, resource termination, sandbox failure, helper loss, shutdown, or restart, and never promote command staging during recovery.
- Provision the Rust execution image through an authenticated non-executing copy operation that excludes credentials and package-manager configuration, and give each command private writable Cargo state seeded from immutable public cache data.
- Normalize command streams into bounded plain UTF-8, strip terminal and bidirectional controls, map known host roots to synthetic names, and publish no raw or live sandbox output.
- Give every committed call a durable result, terminate known interrupted tool loops without continuation, and preserve an unprovable mutation as an acknowledged-only uncertainty blocker.
- Enforce path, depth, count, per-file, total-byte, manifest, staging-growth, and concurrency limits before and during import.
- Use SQLite's online backup API, protect backup files like authoritative data, and distinguish database recovery from workspace recovery.
- Use scoped, server-validated durable cursors and gap-free snapshot-plus-subscription semantics.
- Bind assistant deltas to an exact session and run with a bounded run-local sequence, emit them only after the active transition, and replace them with the committed complete assistant message.
- Keep token deltas, heartbeats, and temporary progress non-authoritative so their loss cannot corrupt recovered state.
- Bound every subscriber queue and disconnect slow consumers without blocking other clients.
- Isolate every session's mutable workspace and execution context, and authorize all cross-resource access explicitly.
- Serialize authoritative session mutations and enforce per-session and global execution limits.
- Make session lifetime independent of client attachments and require explicit authorized cancellation.
- Return deliberate sanitized protocol DTOs rather than persistence records or privileged subsystem payloads.
- Keep authenticated local IPC as the only current application transport and require a separate architecture decision before adding a network listener.
- Start only the exact packaged server companion without a shell or untrusted path selection, pass a reviewed minimal non-secret environment, and establish readiness only through the complete authenticated IPC boundary.
- Let the lifetime host lock resolve concurrent startup, keep client exit independent from server and run lifetime, and require an authenticated graceful-shutdown mutation instead of process-identifier signaling.
- Render untrusted content only through bounded terminal-safe cells that exclude terminal controls and bidirectional formatting, and restore terminal ownership through a scoped guard.
- Hold credential input only in a bounded non-echoing zeroizing client buffer, exclude it from history and presentation, and prohibit automatic credential retries after unknown outcomes.
- Keep protocol and durable encodings independent of native layouts and processor word size, reject integer truncation, and require native release tests on `x86_64` and `aarch64`.
- Keep control files, provider credential state, and host IPC inaccessible from untrusted execution sandboxes.
- Exclude keys, nonces, proofs, and credentials from logs, audit payloads, prompts, environments, registrations, and endpoint names.

## Residual risks

- Root, LocalSystem, administrators, and equivalent privileged accounts can bypass local controls.
- Processes running as the same operating-system user are not distinguished from the legitimate CLI.
- Process separation alone does not sandbox untrusted commands; the selected operating-system policy and candidate-publication boundary must both hold.
- Compromise of the local authentication key permits impersonation until an explicit offline key replacement invalidates existing registrations and endpoints.
- Operating-system, filesystem, storage-device, SQLite, or cryptographic-randomness failures can invalidate durability and authentication assumptions.
- SQLite transactions cannot atomically commit external effects or mutable workspace files, so unresolved effects must remain interrupted or uncertain.
- Atomic file replacement does not create an atomic transaction with SQLite, and same-user interference can still race a validated worktree operation outside Morons' local isolation guarantee.
- Repository import cannot provide a filesystem-atomic snapshot of a source tree changed concurrently by an authorized same-user process; the immutable baseline defines the exact bytes Morons accepted.
- Database backups do not recover workspace changes unless a separately bound workspace snapshot exists.
- Database confidentiality at rest depends on operating-system access controls and storage encryption rather than application-level encryption.
- Logical deletion and page reuse do not guarantee forensic erasure from filesystems, devices, or backups.
- Authorized same-user processes can read owner-controlled session and credential data, deny service, consume provider balances, or interfere with lifecycle state.
- Provider confidentiality depends on OpenCode, its upstream providers, their current policies, and deliberate context selection; submitted repository and conversation content leaves the local trust boundary.
- Provider availability, entitlement, pricing, retention, model behavior, and upstream routing can change independently of a Morons release.
- Ordinary certificate validation does not protect against compromise of the provider, a trusted certificate authority, the operating system, or the local server process.
- Owner-only credential files rely on operating-system access controls and storage encryption and do not provide forensic erasure or protection from the owning user, administrators, crash dumps, or a compromised server.
- Terminal emulators, accessibility services, screen capture, clipboard managers, crash dumps, and same-user processes may observe displayed content or credential keystrokes outside the application's guarantees.
- Native CI and release hardware reduce but cannot eliminate processor, firmware, emulator, compiler, runner-image, sandbox-policy, or operating-system isolation defects.
- The macOS command boundary depends on a deprecated Seatbelt interface that Apple may remove or change; command execution must then remain unavailable until another reviewed backend exists.
- Network-denied offline execution cannot build dependencies absent from the provisioned image, and candidate copying and validation consume bounded but potentially substantial time and disk.
- A sandboxed command can still exhaust its allowed resources, corrupt its discardable candidate, or exploit a kernel or sandbox defect; successful confinement does not make repository code or build output trustworthy.
