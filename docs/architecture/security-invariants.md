# Security invariants

## Local IPC trust boundary

- Protocol-version negotiation is compatibility checking, not authentication or authorization.
- Local transport authentication must complete before either process exchanges application protocol messages.
- The server must authorize the operating-system peer before reading bytes from it or disclosing authentication challenges.
- The client must authenticate the connected server before sending application messages, credentials, repository data, or capabilities.
- Authentication failures must close the connection without an application protocol response.
- Automatic server startup is allowed only after secure control-state classification; malformed, insecure, authentication-failed, or protocol-mismatched state must fail closed without replacement.
- A client-started server must be the exact packaged companion executable, launched without a shell, untrusted path selection, sensitive arguments, or inherited repository, provider, credential, proxy, certificate, or dynamic-loader state.
- A spawned process, process identifier, exit status, or readiness string is never server-authentication evidence; readiness requires the complete registered endpoint, peer authorization, mutual proof, and protocol negotiation boundary.
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
- Assistant deltas must identify an exact session and run, use a bounded run-local monotonic sequence, follow the durable active transition, and be replaced by a complete committed assistant message.
- Per-subscriber queues must be bounded, and slow consumers must be disconnected rather than permitted unbounded memory growth.
- Authenticated local IPC is the only current application transport; a network listener requires a separate architecture decision and threat-model update.

## Provider credentials and model egress

- Only trusted server code may read provider credentials or attach them to an outbound request.
- Persistent provider credentials must reside in a dedicated owner-controlled credential root separate from IPC control state, SQLite data, backups, configuration, workspaces, and runtime directories.
- Provider credentials must never appear in command arguments, environments, SQLite, backups, request fingerprints, audit facts, registrations, model prompts, workspaces, sandbox files, protocol responses, errors, or logs.
- A missing credential is an unconfigured provider; an existing malformed, insecure, unsupported, unreadable, or ambiguously replaced credential state must fail closed rather than be treated as missing.
- Credential input may cross local IPC only after operating-system peer authorization, mutual authentication, and protocol negotiation complete, and secret-bearing types must redact debug output.
- The terminal client must collect credentials without echo, retain them only in a bounded zeroizing transient buffer, and never place them in input history, client configuration, clipboard output, command arguments, environments, logs, panic output, or rendered cells.
- Credential-bearing mutations must never be retried automatically after an unknown outcome; the client must discard the secret and inspect non-secret status before requesting deliberate re-entry.
- Credential application services may configure, replace, remove, or report non-secret status and generation, but they must never return credential bytes or credential-derived fingerprints.
- Credential replacement and removal must use expected-generation checks, atomic owner-only publication, durable non-secret recovery markers, and no automatic retry after an unknown outcome.
- Every run must record the accepted credential generation, and each provider dispatch must fail before network transmission when that generation is no longer current.
- Production provider requests must use server-selected fixed HTTPS origins and paths; clients, repositories, configuration, model output, catalogs, and provider responses must not override an origin, protocol, credential scope, or inference route.
- Redirects must be disabled, certificate and hostname verification must remain enabled, and provider authorization headers must be scoped to the exact reviewed inference origin.
- A remote model catalog may reduce availability but must never enlarge the reviewed built-in service, model, protocol, capability, limit, or data-use manifest.
- The reviewed model manifest must exclude models documented for training, contributor programs, trials, or improvement.
- Provider requests, headers, response bodies, SSE records, decoded fields, accumulated output, tool arguments, identifiers, and errors must be bounded, strictly decoded, and sanitized before crossing the application boundary.
- Provider response identifiers and continuation data are ephemeral run state and must not become authoritative session or recovery state.
- Provider tool definitions come only from a fixed versioned server-owned catalog; clients, repositories, model output, configuration, and remote catalogs cannot add a tool or enlarge its capability.
- Model-selected tool names, call identifiers, arguments, paths, and output remain untrusted and must strictly decode into one offered concrete tool before any call from that provider response is dispatched.
- Repository content may enter a provider request only through bounded canonical context or a bounded committed tool result for the selected authorized run; tools must never attach provider credentials.
- A dispatched inference request must never be retried automatically because an uncertain outcome may already have incurred usage or billing.
- Provider failures and cancellations must preserve prepared, dispatched, outcome, and uncertainty facts without storing credentials or raw provider payloads.

## Session isolation and lifecycle

- One authoritative server may manage many sessions, but every operation and subscription must be authorized within its session scope.
- Session identity and durable lifetime must not depend on a server process, transport endpoint, or client connection.
- Direct user input must be durably attributed to `LocalOwner` and commit atomically with one new run identity before execution begins.
- An exact input retry must return its original user message and run even when the run is active or terminal; conflicting reuse must fail closed.
- A session must have at most one nonterminal top-level run, and rejected or concurrent input must never enter an implicit queue.
- Every run must bind an explicit reviewed OpenCode service, model, protocol revision, context-policy version, limits, and credential generation.
- IPC clients must not submit assistant messages, tool calls, tool results, run transitions, or terminal outcomes as authoritative facts.
- Client detachment or disconnection must not implicitly cancel an active run or transfer control of a session.
- Cancellation must target an exact session and run, commit durable intent before signaling execution, and become terminal only after controlled execution stops.
- An unresolved tool or workspace effect must block new input until the local owner durably acknowledges the exact uncertain run without changing the effect's uncertain outcome.
- Each session must have an isolated mutable workspace and execution context that cannot access another session's state without an explicit authorized capability.
- A local repository may be imported only by the authenticated local owner into a pristine session through one idempotent server-owned workspace operation.
- Repository import must read but never modify the selected source tree, invoke Git, execute repository code, or send imported content to a provider.
- Repository traversal must remain beneath the validated source root, admit only bounded ordinary UTF-8 directories and regular files, and reject links, reparse points, special files, type changes, path collisions, and resource-limit violations.
- A repository source must not overlap in either direction with Morons application, control, runtime, data, backup, credential, or workspace roots; protected Morons state must never be imported or copied into a worktree.
- Components named `.git` under ASCII case folding and their complete subtrees must not enter a session workspace.
- An imported workspace must contain an immutable baseline and a separate mutable worktree produced from the same validated bytes and bound by a versioned architecture-neutral manifest digest.
- Structured file tools may operate only for the exact active run of a ready imported workspace and only through bounded server-validated worktree-relative UTF-8 paths.
- Tool path resolution and mutation must remain relative to pinned directory handles, reject links, reparse points, alternate streams, special files, identity changes, escapes, and collisions, and never reopen a client- or model-selected host path.
- Worktree reads must be bounded, verify the opened node before and after use, return only typed bounded results, and never expose host-absolute paths.
- Worktree edits require a complete-file digest precondition and unambiguous bounded replacements; file and directory creation is exclusive and never replaces an existing name.
- Mutating file tools must stage private operation-bound state, synchronize it, and publish with an atomic handle-relative replace or no-replace operation before committing success.
- Model-selected commands may execute only through the fixed server-owned `run_command` tool for an exact active run with a ready imported workspace and bound execution-image generation; clients cannot submit arbitrary commands or sandbox policy.
- A sandboxed command must receive only an operation-specific candidate worktree, private scratch and cache state, one immutable server-owned execution image, and the minimum reviewed operating-system runtime surface.
- Command candidates and caches are nonauthoritative, isolated per operation and session, and never shared as writable state across sessions.
- A command may publish filesystem effects only after its complete process tree stops and trusted code copies a bounded admissible candidate into a synchronized clean worktree generation whose pointer commits atomically with the durable command result.
- Cancellation, timeout, output exhaustion, sandbox failure, server loss, and restart must discard or quarantine nonauthoritative command staging and must never promote it automatically.
- Sandboxed command execution must start from a reviewed empty environment, close standard input, use bounded pipes without a PTY, deny network and host-local service access, and expose no provider, package-manager, Git, IPC, shell, cloud, or user credentials.
- The server may launch only the exact packaged `morons-sandbox` helper without `PATH`, shell, repository, configuration, or model selection; its one-shot inherited channel must carry no credential or generic privileged operation.
- Every sandbox descendant must inherit operating-system confinement and must be unable to escape process-tree ownership, inspect or signal host processes, create a weaker namespace, retain background execution, or survive controlled termination.
- Missing, partial, unverifiable, or unsupported namespace, Seatbelt, AppContainer, ACL, process-tree, or resource enforcement must fail closed with command execution unavailable and no unsandboxed fallback.
- Windows native FFI for AppContainer, ACL, process attributes, and Job Objects must exist only in the target-specific internal `morons-windows-native` adapter; unsafe code remains denied elsewhere and the adapter exposes only the closed helper-owned command-launch operation.
- A Windows sandbox process must be created suspended with only dedicated closed-input and bounded-output handles, assigned to its configured non-breakaway Job Object before resume, and must never inherit host standard streams or execute before complete Job ownership.
- Trusted Windows result classification, cancellation, deadlines, output draining, and process-tree ownership must remain in the outer helper; no trusted bootstrap, result file, gate, or authoritative control channel may share the command AppContainer identity or writable namespace.
- Operation AppContainer SIDs and ACL grants may reach only operation-private candidate, cache, and image staging; shared images, packaged executables, control roots, and persistent trusted state must never accumulate operation ACL entries.
- Morons-controlled tools, commands, and kernels must share one session workspace lease and must not retain uncontrolled background access that can race a successor operation.
- Untrusted runtimes, tools, subprocesses, kernels, and sandboxes may receive only the mutable worktree, never the baseline, workspace metadata, workspace root, original source tree, or another session's workspace.
- Repository source and destination paths must not become session identity, authorization evidence, durable public state, model context, audit data, logs, errors, events, or protocol results.
- Temporary runtimes, subprocesses, Python kernels, provider response identifiers, and provider continuation state must not become authoritative session storage or receive control-plane credentials.
- Session mutations and concurrent execution must obey server-enforced serialization, resource, concurrency, time, output, call-count, and budget limits.

## Durable state and recovery

- SQLite is the sole authoritative database for durable session, run, idempotency, event, compaction, and audit state.
- Only the server holding the lifetime host lock may open the authoritative database, and database access must pass through one bounded server-owned storage worker.
- The data, backup, credential, workspace, sandbox-image, and sandbox-operation roots must be owner-controlled, local, link-safe, and separate from local IPC control state; untrusted execution may access only its explicit image and operation staging grants.
- The authoritative connection must verify rollback journaling, `synchronous=EXTRA`, platform-supported full synchronization, foreign keys, untrusted schema handling, defensive mode, disabled extensions, and resource limits before serving operations.
- Durable payloads must be bounded, strictly decoded, and explicitly versioned independently of Rust layouts, SQLite rows, and protocol DTOs.
- Canonical facts and affected projections, idempotency outcomes, delivery events, and audit facts must commit atomically.
- Canonical transcript entries must contain only complete bounded attributed user messages, assistant messages, typed tool calls, and typed tool results in monotonic session-entry order.
- A provider response requesting tools must commit its provider outcome and every validated call before execution; each result must commit before it is supplied to another provider turn.
- Canonical tool entries may contain only concrete versioned built-in inputs and results with repository-relative paths, never raw provider JSON, host paths, temporary names, Rust layouts, debug strings, or raw filesystem errors.
- Partial assistant text must remain ephemeral, must never be replayed, and must be replaced in clients by the complete committed assistant message.
- Session transcript pagination must use an immutable entry high water and return a session-event cursor from the same transaction so snapshot and replay remain gap-free.
- A durable result or event must never be published before its transaction commits, and an unknown commit outcome must never be reported as success.
- External effects require durable prepared, dispatched, and outcome boundaries without holding a database transaction across the effect.
- A repository import must stage beneath its identity-bound workspace, publish one complete repository directory atomically, and report success only when the durable completion fact agrees with the validated baseline, worktree, marker, and manifest.
- Repository-import recovery must never reread the source automatically and may inspect, publish, or remove only exact operation-bound state confined beneath the expected private workspace.
- Tool recovery must never rerun a call or provider turn, reread an interrupted observation, or publish an intended edit; it may reconcile or remove only exact operation-bound state when target identity and before-or-after digests prove the outcome.
- Active worktree generations are server-generated identities; a command result and generation-pointer change must commit in one transaction after the complete clean generation is synchronized and validated.
- Startup must never launch, resume, or repeat a command or promote its unreferenced candidate; it may remove only exact inactive operation-bound staging after process-tree termination is proven.
- Every committed tool call must receive a durable terminal result during normal execution or recovery; an unprovable mutating outcome must terminate the run as uncertain and block new input.
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

## Terminal and local process lifecycle

- Closing a terminal client detaches it and must not cancel a run, stop the server, or change session lifetime.
- Server shutdown requires an explicit authenticated idempotent local-owner mutation, may signal only on first acceptance, and must use graceful run shutdown; clients must not kill a process based only on registration state.
- Concurrent client startup may launch contenders, but only the lifetime host-lock owner may publish or clean control state.
- Untrusted user, provider, catalog, error, repository path, tool argument, tool result, command argument, and command output text must be converted to bounded terminal cells without forwarding control sequences, operating-system commands, hyperlinks, title changes, device controls, or bidirectional formatting controls.
- The terminal may present only committed bounded sanitized command summaries and excerpts, never raw or live sandbox streams, interactive process input, inherited terminal access, or a sandbox PTY.
- Untrusted text must never be written through raw ANSI or terminal-control output paths.
- Terminal mode and screen ownership must be restored on every ordinary exit and handled error path, and restoration diagnostics must not contain credential or transcript buffers.
- The terminal client is not a terminal emulator, PTY, shell, editor, or raw sandbox view and must not expose arbitrary user command submission.

## Processor architecture portability

- Release-supported processor architectures are 64-bit `x86_64` and `aarch64` for the selected macOS, Linux, and Windows targets.
- Protocol, authentication, persistence, fingerprints, and cursors must use explicit widths and byte order and must never serialize pointers, `usize`, host-endian values, native layouts, or processor architecture.
- Conversions among protocol lengths, SQLite integers, filesystem sizes, and `usize` must reject overflow and truncation.
- Processor architecture is never authentication evidence, authorization evidence, a capability, or a reason to weaken limits or security behavior.
- Native tests are required for release support because cross-compilation and emulation cannot prove IPC peer identity, filesystem controls, synchronization, sandbox confinement, process-tree termination, network denial, process lifecycle, or terminal behavior.
- Distribution artifacts must pair client and server executables built from the same revision for one target and must not download or select executables from untrusted state.

## Failure handling and isolation

- Connection, authentication, framing, and application handshakes must have bounded, non-resetting deadlines.
- Authentication records and application frames must have independent size limits and strict decoding.
- Connections admitted before endpoint security and registration publication are complete must be rejected.
- Authentication nonces and proofs must not be accepted more than once or retained after the connection attempt ends.
- Untrusted repository processes must not receive or be able to access the control directory, authentication key, provider credential root, endpoint registration, host IPC endpoint, data root, backup root, workspace baseline or metadata, original source tree, or another session's workspace.
- Tool definitions, model instructions, digest preconditions, path validation, and operation identifiers are not sandbox boundaries; filesystem confinement must be enforced by trusted handle-relative server code and operating-system isolation.
- Authentication and authorization audit events must not contain keys, nonces, proofs, or other authentication material.
