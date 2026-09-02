# ADR 0008: Local repository import and workspace baseline

## Status

Superseded by ADR 0012

## Context

Morons sessions already own server-generated private workspace identities, but those workspaces contain no repository content. A coding run needs a mutable repository copy without granting the agent, tools, subprocesses, or provider access to the owner's original working tree.

A repository tree can contain malicious names, links, reparse points, special files, Git control data, oversized or changing files, and sensitive content. Copying it is a filesystem effect that cannot commit atomically with SQLite. The source path can also disclose local host information and must not become session identity, model context, or a durable public value.

Future file tools, diff review, and controlled export require an immutable comparison point. Depending on the original repository after import would make later review sensitive to unrelated host changes and would permit a session to reach outside its isolated workspace.

## Decision

### Product and authority boundary

An authenticated local owner may import one local repository into a pristine session through a concrete server application mutation. Import is available only while the session has no transcript entries, run facts, active run, uncertainty blocker, or existing repository. A completed import is permanent for that session; refreshing from another source requires a new session.

The mutation contains a stable mutation request identifier, the target session identifier, and one bounded lexically normalized absolute UTF-8 source path without `.` or `..` components. The source path is transient operation input. The server never treats it as session identity or authorization evidence and never returns it in a response or event.

The server performs the import. The terminal client does not enumerate files, transmit repository contents, select destination paths, or retain a repository-path history. Import does not invoke a model, send repository content to OpenCode, validate a credential, run Git, execute a hook, start a repository process, or modify the source tree.

### Source-tree policy

The server resolves and validates the selected source root as an ordinary readable directory. The resolved source must not equal, contain, or be contained by the Morons application, control, runtime, data, backup, credential, or workspace roots. Selection fails rather than skipping a protected Morons subtree, so server-managed credentials, authentication material, databases, backups, baselines, and other workspaces cannot be imported.

Repository traversal admits only ordinary directories and ordinary regular files. It rejects symbolic links, junctions, mount-point reparse entries, other reparse points, sockets, devices, FIFOs, and every other special entry. A type or identity change between validation and opening fails the import.

Every admitted relative path must have bounded depth and encoded length, contain only valid nonempty UTF-8 components, and remain beneath the validated source root. `.` and `..` are never imported components. Destination creation uses only validated relative components beneath a server-generated staging directory and uses exclusive creation so case-folding, normalization, or duplicate-name collisions fail closed.

Every component equal to `.git` under ASCII case folding is omitted with its complete subtree. Git object databases, worktree indirection, hooks, configuration, credential helpers, remotes, alternates, and locks therefore do not enter the session workspace. Files such as `.gitignore` and `.gitattributes` remain ordinary repository content.

The importer copies file bytes, not source links, ownership, ACLs, extended attributes, alternate streams, sparse layout, or special permission bits. Destination directories receive private workspace controls. Destination files receive private controls; on Unix the mutable copy retains only the source owner's executable bit when it was set. Hard-linked source files become independent destination files.

The server checks file identity and size before and after each copy and rejects a file that changes while being read. The imported snapshot is the exact validated byte stream written by the importer; it is not represented as an atomic snapshot of a concurrently changing source filesystem. Morons does not launch repository processes against the source tree, and same-user processes modifying the source concurrently remain outside the local isolation guarantee.

### Workspace layout and baseline

The private session workspace retains its existing server-generated identity file. A repository import is published as one identity-bound repository directory containing:

- an immutable baseline tree;
- a separate mutable worktree; and
- bounded versioned import metadata binding both trees to the session workspace and import operation.

The importer reads each source file once and writes the same bytes into the staged baseline and staged worktree. It computes a canonical architecture-neutral manifest ordered by raw UTF-8 bytes of `/`-joined relative components without Unicode normalization. Manifest records use explicit fixed-width big-endian lengths and sizes and SHA-256 file-content digests. The completed durable fact records the manifest digest, file and directory counts, total logical bytes, format version, and import operation identity, but not the source path or file contents.

The complete repository directory is built under an operation-specific private staging name, synchronized, marked complete, and atomically renamed to its final workspace-relative name. The final name is never selected by the client or repository. Unexpected final or staging state fails closed.

The baseline is server-owned comparison authority, not agent input; its repository content remains untrusted. Future runtimes, tools, kernels, and sandboxes may receive access only to the mutable worktree. They must not receive the workspace identity, baseline, import metadata, workspace root, source repository, another session's workspace, SQLite state, control state, or credentials.

No operation writes changes back to the source repository. Future export must be a separate explicit local-owner operation that compares the mutable worktree with this baseline and defines its own destination, conflict, and uncertainty semantics.

### Durability, idempotency, and recovery

Repository import uses the prepared, dispatched, outcome, idempotency, audit, and publication boundaries from ADR 0003. The accepting transaction verifies local-owner authority and pristine session state, binds the session and workspace identities, records a normalized fingerprint, creates the import operation, and makes the session workspace busy before any copy begins.

The normalized operation fingerprint binds the session, operation, and exact submitted path bytes without storing that path. Durable import and audit facts contain only server-generated identifiers, workspace state, bounded counts, versions, and digests. Logs, errors, events, and debug output do not contain the source path or repository content.

A dispatched import never causes the server to read the source tree again automatically. An exact request retry resolves durable operation state and cannot start a second copy. A client that observes an unknown outcome retains the same non-secret mutation identity and reloads workspace state before offering an exact retry or abandonment.

Startup recovery performs no source read. It inspects only operation-bound state beneath the expected private workspace:

- prepared but undispatched state is finalized as not applied;
- incomplete unpublished staging state without a valid completion marker is removed only by its exact confined staging identity and finalized as not applied;
- a complete staged or published repository whose marker, workspace identity, operation identity, manifest, baseline, and worktree agree is published when necessary and finalized as completed;
- mismatched, ambiguous, linked, out-of-scope, or unexpectedly published state fails closed and leaves the session workspace blocked for explicit recovery.

A database result or marker alone is not proof of a completed import. Completion requires the database fact and the validated published repository to agree. Import success and the durable session event are published only after the completion transaction commits. After completion, worktree changes are expected; later validation continues to bind the immutable baseline and metadata but does not require the mutable worktree to retain its import digest.

### Limits and scheduling

The server enforces fixed versioned limits before and during import for source-path bytes, path depth, relative-path bytes, entry count, individual file bytes, total logical bytes, manifest bytes, staging disk growth, and concurrent imports. Limit exhaustion stops copying, performs only confined operation-specific cleanup, and returns a structured resource failure without publishing a repository.

Import runs as a bounded server-owned workspace operation outside the SQLite worker and outside client connection lifetime. It never holds a database transaction while traversing or copying files. Client detachment does not transfer operation ownership or expose staging state. Graceful server stop prevents new imports and applies the same recovery boundary to an import that cannot finish before shutdown.

### Application and terminal contract

The session snapshot and durable session-event stream expose a sanitized workspace summary with one of these states:

- empty;
- importing;
- ready, with bounded file count and logical byte count; or
- blocked, with a stable non-path failure classification.

Clients validate that workspace events match the selected session and follow legal state transitions. Repository names, source paths, destination paths, manifest digests, file names, and file contents are not part of this summary.

The Ratatui application accepts the bounded path in a modal plain-text field, renders it only through the terminal-safety boundary, and does not add it to prompt or credential history. Before submission it requires explicit confirmation that every admitted regular file except `.git` control data will be copied and that no change will be written back automatically. Submission clears the field; one redacted bounded pending-mutation value may retain the exact path only until a committed result, explicit abandonment, or client exit so an exact idempotent request can be resolved after transport loss. Presentation remains non-authoritative; the server repeats every validation.

### Validation

Implementation requires deterministic and real-filesystem tests covering:

- authenticated local-owner scope, pristine-session enforcement, exact retry, conflicting reuse, and concurrent import rejection;
- ordinary nested files, empty directories, executable-bit narrowing, `.git` omission, UTF-8 path validation, and deterministic manifest construction;
- links, reparse points, special files, protected Morons root overlap, path escape attempts, source changes, destination collisions, depth, file, count, and byte limits;
- private baseline, worktree, marker, staging, and inherited Windows DACL controls;
- failures and process termination before dispatch, during copy, before marker synchronization, before publication, and before completion commit;
- startup reconciliation of not-applied, complete staged, complete published, mismatched, and ambiguous states without reading the source;
- proof that source paths and repository content do not enter SQLite facts, audit facts, logs, errors, events, debug output, provider requests, or terminal output outside the active path field;
- proof that imported content never changes the source and that future untrusted execution receives only the mutable worktree; and
- native macOS, Linux, and Windows tests on supported `x86_64` and `aarch64` targets.

## Consequences

- A session can own a repository snapshot without depending on the original path after import.
- The immutable baseline gives future diff review and controlled export a stable local authority.
- Excluding Git control data prevents repository hooks, configuration, and credentials from becoming trusted workspace behavior.
- Rejecting links and non-UTF-8 names intentionally narrows the initial supported repository subset while remaining a permanent safe import capability.
- Baseline and worktree copies consume additional disk space in exchange for deterministic review and source independence.
- Import alone does not give a model repository context or tool capability; structured tools remain a separate server-owned implementation section.
