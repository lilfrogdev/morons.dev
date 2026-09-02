# ADR 0010: Sandboxed command execution and worktree generations

## Status

Accepted

## Context

Structured worktree tools let a model inspect and edit bounded UTF-8 files, but Morons cannot invoke a compiler, test runner, formatter, or other repository process. Building Morons from its own imported repository requires native Rust tools and execution of untrusted dependencies, build scripts, procedural macros, compiler output, and model-selected commands.

Repository code, command arguments, executables, toolchains, package caches, process output, and resulting files are malicious. A child process running with the server's ordinary host authority could read credentials and control state, reach local services or the network, signal the server, modify another session, survive cancellation, or corrupt the authoritative worktree between a database fact and a crash.

ADRs 0003, 0005, 0008, and 0009 require durable calls and results, no blind replay, an immutable baseline, one session workspace lease, and no sandbox access to server-private state. Command execution must satisfy those boundaries without adding a user-facing terminal, PTY, arbitrary client command endpoint, privileged proxy, or temporary host-access mode.

## Decision

### Capability and authority boundary

The fixed server-owned model tool catalog gains one `run_command` tool. It executes one bounded structured process request inside an enforced sandbox for the exact active run of a ready imported workspace and a ready compatible execution image. The tool is not offered when either prerequisite or the native sandbox backend is unavailable. Clients, repositories, configuration, remote catalogs, and model output cannot add a command capability or weaken its sandbox policy.

A command input contains:

- one bounded executable name resolved only from the selected server-owned execution image;
- bounded UTF-8 argument values;
- one bounded worktree-relative working directory; and
- no shell string, host path, environment override, standard-input content, network option, privilege option, mount option, or sandbox-policy field.

The server passes arguments as an operating-system argument vector. It does not interpolate them into a shell string. Executables launched by an admitted command may themselves invoke shells, compilers, linkers, build scripts, tests, and descendants inside the same sandbox. Program-name restrictions are product semantics, not a security boundary; the operating-system sandbox contains every descendant.

Only validated model output may request `run_command`. Authenticated IPC clients cannot submit authoritative command calls or command results, and the Ratatui application does not expose arbitrary user command entry, a terminal emulator, a PTY, or raw process interaction.

### Rust execution image

The initial execution environment is one concrete server-owned Rust image sufficient to run native `cargo` and `rustc` operations without network access. The image contains a local-owner-provisioned Rust toolchain, its native runtime requirements, and a bounded public Cargo registry seed. It never contains Cargo credentials, package-manager configuration, SSH state, Git credentials, provider credentials, IPC authentication material, user shell configuration, or arbitrary home-directory content.

Provisioning is an authenticated idempotent local-owner operation. It copies admitted ordinary files and directories into a dedicated private sandbox-image root without executing the selected toolchain or repository code. Submitted source paths are transient, are not authority, and never enter model context, command output, audit facts, events, or protocol results. Persistent image state records a server-generated image identity, operating-system and processor target, format and limits versions, bounded counts and sizes, and an architecture-neutral manifest digest.

The image is immutable while commands use it. Replacement creates and verifies a new image generation before making it current. A command binds the exact image generation accepted for its run. Image content remains untrusted executable input and is never loaded into the trusted server process.

Image provisioning uses prepared, dispatched, completion, idempotency, audit, staging, synchronization, and generation-pointer boundaries. Its request fingerprint binds the exact submitted source-path bytes without storing them. After dispatch, startup and exact retries never reread the toolchain or Cargo source automatically; recovery inspects only exact operation-bound image staging and either validates a complete synchronized image generation or records a non-success outcome.

Each command receives a private writable Cargo home seeded from the image's public registry data. Caches are not shared writable state across sessions or command operations. Cargo runs offline and cannot consult host Cargo configuration or credentials. Missing dependencies fail as a bounded command result rather than enabling network access or a privileged package-manager proxy.

Additional language environments require later reviewed catalog and image-format additions. A generic client-supplied executable root, host `PATH`, host home mount, or mutable cross-session package cache is not introduced.

### Private candidate worktree

A command never executes against the authoritative mutable worktree directly. Under the session workspace lease, trusted server code copies the active worktree generation into an operation-specific candidate tree beneath a dedicated sandbox root. The candidate contains repository bytes only; it does not contain or expose the immutable baseline, workspace identity or metadata, generation metadata, original source, another generation, another session, SQLite state, backups, credentials, control state, runtime endpoints, or the execution-image source paths.

The active worktree is represented by a server-generated generation identity stored in authoritative state. Existing imported worktrees are migrated through an identity-bound recoverable layout operation before command execution becomes available. Structured tools always resolve the current committed generation while holding the same workspace lease.

The sandbox may read, write, create, delete, rename, and execute within its candidate and private scratch roots. It may read and execute the immutable Rust image and the minimum reviewed operating-system runtime paths. Every other host filesystem location is denied. The server supplies no absolute host path through command metadata or environment, argument strings never grant filesystem access, and known sandbox, image, and runtime prefixes are mapped to stable synthetic names before any output becomes durable or leaves the server.

A normal command exit, including a nonzero exit code, is eligible to publish candidate changes. Trusted code first proves that the complete process tree stopped, then copies only a bounded admissible tree into a clean operation-bound worktree generation. Promotion admits bounded UTF-8 paths, ordinary directories, and ordinary regular files, applies private controls, strips untrusted ownership and auxiliary metadata, synchronizes files and directories, and computes a canonical manifest. Links, reparse points, alternate streams, special files, identity changes, path collisions, unsupported names, and resource-limit violations reject the candidate without changing the active generation.

The clean generation is synchronized and marked complete before one SQLite transaction atomically commits the command result, terminal operation facts, transcript entry, audit and delivery facts, and active-generation pointer. Only that pointer commit publishes command filesystem effects. An unreferenced candidate or clean generation is not workspace state and is removed through exact operation-bound recovery. Inactive old generations are deleted only after no operation can retain them.

Cancellation, timeout, output-limit termination, process-limit termination, sandbox failure, server shutdown, runner loss, or server restart never promotes the candidate. This deliberately gives interrupted commands no authoritative workspace effect. Startup never promotes a candidate merely because a command or copy appears to have completed.

### Packaged sandbox runner

The server launches an exact packaged `morons-sandbox` helper through an inherited one-shot control channel. The helper is an internal implementation component rather than a public command surface or generic privileged service. It receives no provider credential, IPC key, endpoint registration, database handle, baseline handle, source-tree handle, or authority to select additional paths.

The one-shot launch specification binds the operation, candidate, scratch root, execution-image generation, executable, arguments, working directory, policy version, limits version, and inherited standard-stream handles. Model and repository values cannot supply an authoritative host path or serialized sandbox policy. The helper starts from a reviewed empty environment and constructs only fixed non-secret variables such as synthetic home and temporary directories, fixed image `PATH`, locale, plain-output controls, and offline Cargo settings.

The helper creates the sandbox before the requested executable begins. A watchdog inherited from the server and platform process-tree containment ensure that server loss, cancellation, timeout, or helper failure applies uncatchable whole-tree termination, reaps the controlled child, and prevents every descendant from executing again. The server does not accept an exit status until the helper proves the complete sandbox tree is stopped.

A platform whose complete policy cannot be installed, verified, and exercised fails closed with command execution unavailable. There is no unsandboxed fallback, approval bypass, environment-only network suppression, or host-permission mode.

### Platform enforcement

On Linux, the helper uses new user, mount, PID, and network namespaces, a synthetic minimal mount view, an isolated `/proc`, dropped capabilities, `no_new_privs`, Landlock filesystem rules, and seccomp filters. The candidate, scratch space, private Cargo home, immutable image, and reviewed runtime files appear at synthetic paths. The network namespace has no external interface. Seccomp denies host process inspection and signaling, namespace or mount creation after setup, privilege changes, keyrings, kernel interfaces, raw and Internet sockets, and bypass paths such as `io_uring`; process-local inherited pipes and bounded local `socketpair` use remain available. Required namespace, Landlock, and seccomp enforcement is probed and must be fully active.

On macOS, the helper invokes only `/usr/bin/sandbox-exec` with a generated default-deny Seatbelt profile. The profile allows process execution and forking inside the sandbox, signaling and process inspection only within the same sandbox, read-write-execute access only to candidate and scratch roots, read-execute access only to the image and reviewed runtime roots, and no outbound, inbound, loopback, Mach-service, TCC, device, keychain, or unrelated user-data access beyond the minimum explicitly tested runtime services. Descendants inherit the profile and cannot leave the runner-owned process group. Absence or changed behavior of the deprecated Seatbelt interface fails closed and prevents release support on that macOS version until reviewed.

On Windows, the helper creates an operation-specific AppContainer with no network capability and launches it in a Job Object configured to prevent breakaway and kill the complete process tree when closed. Candidate, cache, and read-only image views are operation-private copies beneath AppContainer-accessible staging; the shared immutable image and packaged installation never receive operation SIDs or mutable ACL entries. Morons private roots retain their owner-and-LocalSystem controls and are never granted to the container. The environment contains only reviewed machine requirements and synthetic sandbox directories. AppContainer profile, ACL, Job Object, process-attribute, and cleanup failures fail closed.

Windows process creation and ACL APIs require native FFI that the standard library cannot express. One target-only internal `morons-windows-native` library is the complete unsafe-code boundary for these calls and uses only pinned Windows binding definitions rather than a generic sandbox runtime. Unsafe code remains denied in every other Morons crate; the adapter denies unsafe operations in unsafe functions, denies undocumented unsafe blocks, and documents the handle, pointer, buffer, ownership, and lifetime preconditions of every unsafe block. It exposes no binary, generic launcher, arbitrary SDDL, capability name, process flag, environment entry, handle, path grant, or client/model-callable operation.

The trusted helper gives the adapter one closed command-launch value containing only validated operation-owned path roles, the exact executable resolved inside the private image view, structured arguments, the candidate-relative working directory, and fixed limits. The adapter constructs the synthetic environment and dedicated closed-input and bounded-output handles itself. It creates the requested AppContainer process suspended with an explicit security-capabilities attribute and exact handle list, creates and configures a non-breakaway kill-on-close Job Object, assigns the suspended process, and only then resumes its first thread. It returns only trusted parent pipe endpoints and opaque process-tree ownership to the helper, never starts untrusted code before Job ownership is established, and never falls back to inherited host standard handles.

No trusted bootstrap, result file, gate, control channel, or authoritative status logic runs inside the command AppContainer. The trusted outer helper drains the dedicated pipes, enforces cancellation and deadlines, classifies the root exit, terminates unexpected descendants, and verifies through Job accounting that no active member can execute before returning a result. Untrusted processes therefore share neither an identity nor a writable control surface with the code that decides the command result.

Every backend denies access to the Morons local IPC endpoint even though application authentication would independently reject a process without the key. Sandbox confinement, not endpoint secrecy or prompts, is the boundary.

### Output and result contract

Standard input is closed. Standard output and standard error are independent bounded pipes drained concurrently by trusted code. There is no inherited terminal or PTY. Exceeding an output, inactivity, or total deadline terminates the complete process tree and discards the candidate.

Before persistence, provider context, events, logs, or terminal presentation, trusted code converts output into bounded plain UTF-8, removes terminal controls and bidirectional formatting controls, maps known host prefixes to synthetic paths, and records explicit stream and truncation classifications. Invalid bytes receive a deterministic escaped representation rather than reaching a terminal or provider as raw bytes.

A durable command result contains only the command-call identity, admitted executable name, sanitized exit classification and fixed-width exit value when available, bounded sanitized output, candidate publication status, and stable resource or sandbox failure classifications. It does not contain process identifiers, host paths, environment values, raw operating-system errors, sandbox profiles, image source paths, temporary names, or native status layouts.

The terminal may render only a concise committed command summary and bounded sanitized output excerpts through the existing terminal-safety layer. Live raw command streaming, ANSI interpretation, interactive prompts, and terminal passthrough are not part of this capability.

### Durable ordering and recovery

`run_command` is a typed tool in the existing durable multi-turn loop. The complete provider response and every call commit before execution. A command operation records prepared and dispatched facts before launching the helper, and its result commits before another provider turn.

Prepared state binds the active worktree generation and manifest, candidate identity, execution-image generation, command-policy and limits versions, and normalized command specification. It never contains host paths, environment values, image source paths, or credentials. The session workspace lease remains held from candidate creation through process termination, validation, publication or discard, and terminal result commit.

Startup never launches, resumes, or repeats a command. A committed call without dispatch receives a not-dispatched result. A dispatched command without a transaction that published a new generation receives an interrupted result, and only its exact nonauthoritative staging may be quarantined or removed. A committed active-generation pointer and command result are validated together; one without the other is canonical corruption and fails closed.

Because untrusted execution can modify only a nonauthoritative candidate, a stopped or crashed command does not create workspace uncertainty. An inability to prove runner termination or confine an orphan is a sandbox-host failure that prevents new command execution and server startup from claiming the sandbox is healthy; it never makes the candidate authoritative.

### Limits and scheduling

Command execution has fixed versioned limits for arguments, working-directory depth, provider calls, commands per turn and run, candidate entries and bytes, image and cache bytes, process count, memory, CPU, wall time, inactivity, file size, output per stream, aggregate output, clean-generation size, and global concurrent sandboxes. Every length and durable value uses checked fixed-width conversion independent of processor architecture.

One command consumes the run's global execution permit and one separately bounded sandbox permit. Commands execute sequentially with structured tools under the session workspace lease. No background process, daemon, file handle, candidate, or writable cache may outlive its operation.

Command execution is network-denied in this decision. Package download, arbitrary egress, local-service access, credential injection, interactive approval, and a privileged network proxy require separate architecture decisions.

### Validation

Implementation requires native adversarial tests on every supported operating system and processor architecture covering:

- exact tool schema, program and argument bounds, model-only invocation, image-generation binding, and rejection before dispatch of malformed or excessive calls;
- inability to read or write the baseline, metadata, original source, server roots, credentials, control state, host IPC, user home, another candidate, or another session;
- inability to use Internet, loopback, Unix-domain, named-pipe, raw, packet, or inherited communication to reach host services;
- descendant sandbox inheritance, process-group or Job Object containment, daemon and namespace escape attempts, host process signaling, cancellation, timeout, output exhaustion, helper loss, and server termination;
- native Cargo check, build, test, and Clippy fixtures using the immutable offline Rust image, including malicious build scripts and procedural macros;
- candidate changes after zero and nonzero exits, discard after forced termination or restart, clean-generation validation, atomic pointer publication, projection rebuild, and old-generation cleanup;
- links, junctions, reparse points, alternate streams, special files, non-UTF-8 names, collisions, oversized artifacts, changing entries, ACL changes, and executable files in candidate output;
- output controls, invalid bytes, path disclosure, stream deadlock, rendering bounds, and absence of raw sandbox output from facts, events, errors, logs, provider input, and terminal cells;
- exact recovery at every prepare, dispatch, helper, candidate, clean-copy, synchronization, pointer-commit, result-commit, and cleanup seam without command replay or unintended promotion; and
- unavailable or partially enforced namespace, Seatbelt, AppContainer, ACL, process-tree, and resource controls failing closed without an unsandboxed fallback.

## Consequences

- Morons can compile and test code, including itself, while provider credentials and authoritative workspace state remain outside untrusted execution.
- Successful commands can persist validated filesystem effects through an atomic worktree-generation pointer rather than exposing the authoritative tree during execution.
- Interrupted commands are discardable and do not require guessing which arbitrary writes reached the active workspace.
- Copying and validating worktree generations, provisioning an offline Rust image, and maintaining native sandbox backends add disk, startup, implementation, and test cost.
- Network-denied offline execution intentionally fails when required dependencies are absent from the provisioned image.
- Raw shells, PTYs, user command entry, live terminal output, package download, Git credentials, and network egress remain unavailable.
- Diff review and controlled export remain required before a complete self-hosting workflow can move changes or built artifacts out of the session workspace.
