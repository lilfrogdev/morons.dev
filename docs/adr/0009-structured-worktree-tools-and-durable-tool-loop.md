# ADR 0009: Structured worktree tools and durable tool loop

## Status

Proposed

## Context

Repository import gives a session a private immutable baseline and a separate mutable worktree, but the model cannot inspect or change that worktree. Morons is intended to be a local coding agent rather than only a durable chat application.

Model-selected paths, tool names, arguments, and output are untrusted. A worktree can also be changed by a prior tool, a future sandboxed process, or same-user interference. A generic filesystem endpoint, host path, shell command, or raw workspace capability would expose the baseline and server state and would bypass the durable run lifecycle established by ADRs 0003 and 0005.

The first coding capability needs to be useful without introducing command execution, Git execution, an editor, a plugin system, or a temporary tool protocol. It must preserve deterministic transcript history, bounded provider context, cancellation, crash recovery, and the repository boundary established by ADR 0008.

## Decision

### Capability and authority boundary

The trusted server owns one fixed built-in catalog of structured worktree tools. The initial catalog contains exactly:

- `list_directory`, which lists one directory page;
- `read_file`, which reads a bounded UTF-8 line window and reports the complete file digest;
- `search_text`, which performs bounded literal UTF-8 search beneath one worktree-relative directory;
- `edit_file`, which applies bounded exact-text replacements to an existing UTF-8 file after a digest precondition;
- `create_file`, which creates one UTF-8 file without replacement beneath an existing directory; and
- `create_directory`, which creates one directory without replacement beneath an existing directory.

These are permanent concrete built-ins, not aliases for arbitrary filesystem operations. The catalog does not include file deletion, rename, Git, shell commands, process execution, glob or regular-expression engines, binary writes, host file access, baseline access, generic plugins, or client-supplied tool definitions.

Tools are offered to a provider turn only when the selected reviewed model supports tool calls and the session has a ready imported repository. A server-owned versioned developer instruction describes the catalog and the fact that paths are relative to the worktree. Prompts and tool annotations are usability input, not a security boundary.

An authenticated IPC client may submit local-owner user input and cancellation intent but cannot submit a tool call, tool result, tool operation, workspace path, or run transition as an authoritative fact. Model output requests tools; trusted server code validates, authorizes, executes, and records them.

Starting a run with a ready workspace authorizes the selected OpenCode service and model to receive only the bounded worktree content returned by tools during that run. Import itself still performs no provider request. Tools never attach a provider credential or expose local authentication material.

### Worktree-relative path contract

Every tool path is a bounded UTF-8 string in a server-defined slash-separated relative format. The exact string `.` denotes the worktree root for read-only directory-scoped tools. Every other path consists of nonempty components and is rejected if it is absolute, contains an empty, `.` or `..` component, contains NUL, backslash, or colon, exceeds the component, depth, or encoded-length limits, or cannot be represented as exact native child names.

The server does not Unicode-normalize, case-fold, canonicalize, or repair tool paths. It never accepts a host-absolute path from a tool call and never returns an absolute workspace path to the model, client, transcript, event stream, error, audit fact, or log. A relative path is repository content and may be recorded only in the bounded canonical tool entries and exact operation-recovery facts that require it; structured audit facts use tool kind, operation identity, and a path digest rather than path text or file content.

The server resolves the session and worktree from the run and server-owned workspace identity. It pins the worktree root, walks each component relative to validated directory handles without following links or reparse points, and opens the final node with the minimum rights needed by that concrete tool. Unix traversal uses handle-relative no-follow operations. Windows traversal and mutation use no-follow handle-relative operations and reject alternate streams and reparse points.

Every traversed node must be an ordinary directory or regular file of the expected kind. Symbolic links, junctions, mount-point reparse entries, other reparse points, special files, changed identities, escapes, and unexpected workspace metadata fail closed. Destination parents are resolved before mutation, and creation or replacement occurs relative to the pinned parent rather than by reopening a concatenated host path.

The immutable baseline, import metadata, workspace identity, workspace root, original source repository, other sessions' worktrees, data and backup roots, credential root, control state, and runtime endpoints are never in the tool capability graph.

### Read-only tools

`list_directory` returns entries sorted by raw UTF-8 bytes. Each entry contains only its child name and ordinary file-or-directory kind. Results are page-bounded and report whether more names remain. Directory changes between calls may change later pages; a tool listing is not an authoritative filesystem snapshot.

`read_file` opens one pinned ordinary file, enforces a fixed file-size and output bound, validates UTF-8, hashes the complete byte stream with SHA-256, and returns a bounded one-indexed line window with its line range, end-of-file state, and complete-file digest. It verifies the opened file's identity, type, size, and supported change metadata after reading. Binary or changing files produce typed tool errors without returning partial bytes.

`search_text` performs literal matching without interpreting repository configuration, `.gitignore`, glob syntax, or regular expressions. It traverses ordinary entries in raw UTF-8 byte order beneath the selected relative directory, skips binary file content with a bounded count, and returns bounded relative paths, one-indexed line numbers, and bounded matching line fragments. Traversal, bytes scanned, files scanned, matches, output, and time are independently bounded. Truncation is explicit.

Read-only tool results are observations, not workspace effects. They are committed before another provider turn but are never recreated after a crash because the observed worktree may have changed. An interrupted read operation receives a durable interrupted result and does not create a workspace uncertainty blocker.

### File mutations

`edit_file` requires the SHA-256 digest returned by a prior complete-file observation. It reads and validates the current UTF-8 file, requires the digest to match, and applies a bounded ordered collection of exact replacements against the original text. Each nonempty `old_text` must occur exactly once, replacement ranges must not overlap, and duplicate or ambiguous matches fail without writing. An empty source file may use one explicitly bounded empty-file replacement. The resulting file must remain within the fixed file and aggregate run limits.

`create_file` requires an existing pinned parent and creates one ordinary UTF-8 file exclusively; existing names are conflicts and are never replaced. `create_directory` similarly creates one empty ordinary directory exclusively. Neither operation creates missing parents implicitly. New files and directories receive private workspace controls, and edits preserve only the existing owner-executable bit on Unix rather than copying broader metadata.

A file mutation uses one server-generated tool-operation identity. After validation and before dispatch, the server records its operation kind, canonical relative target, target-parent identity, before state, intended after state, content digest, limits version, and operation-specific temporary name. Content bytes and exact replacement text belong only to bounded canonical tool entries, not audit facts or generic operation logs.

The server writes intended file bytes once to an exclusive operation-specific temporary child of the pinned destination parent, applies private controls, synchronizes the file, verifies its digest and identity, and synchronizes required directory state. Dispatch commits immediately before the publication step. An edit atomically replaces the exact target only after revalidating its committed identity and complete metadata. A create atomically renames without replacement. Directory creation uses the equivalent exclusive handle-relative publication. The destination parent is synchronized before success is committed.

Morons serializes controlled operations for one session worktree. A future command or kernel implementation must use the same session workspace lease and may not retain background access after its tool operation ends. Same-user processes independently changing private workspace files remain outside the local isolation guarantee, but handle-relative confinement, identity validation, digest preconditions, and fail-closed reconciliation still apply.

No structured tool writes to the immutable baseline or original source repository. A successful mutation changes only the mutable worktree and does not imply export.

### Provider turns and canonical ordering

A run may contain multiple bounded provider turns. Every turn has a new server-generated provider-operation identity and uses the run's committed service, model, protocol revision, credential generation, context policy, tool-catalog version, source-entry high water, and cumulative limits.

A completed provider response is normalized before any tool dispatch. Tool names must exactly match the offered built-in catalog. Arguments must strictly match that tool's closed schema, contain no unknown or duplicate fields, and decode into a concrete typed input. Provider call identifiers must be bounded and unique within the run. An unknown tool, malformed argument, duplicate call identifier, excess call, contradictory final answer, unsupported output sequence, or invalid aggregate response fails the run before any call from that response is dispatched.

When a valid response requests tools, one transaction:

1. completes the dispatched provider operation and records bounded usage;
2. commits any complete assistant commentary permitted by the response ordering;
3. assigns server-generated tool-call and tool-operation identifiers;
4. appends every validated typed tool call in provider order;
5. updates projections, cumulative run limits, audit facts, and delivery events; and
6. commits before the first tool executes.

Tool calls execute sequentially in committed order. Each call receives one durable typed result, including expected conflicts and bounded operational errors. A result transaction completes the tool operation, appends the canonical tool result, updates projections and limits, and publishes its delivery event. The next provider turn is constructed only after every call from the preceding response has a committed result.

Canonical transcript entries gain explicit versioned tool-call and tool-result variants. They contain server-generated identifiers, run and provider-operation provenance, tool kind, typed bounded repository-relative inputs, result status, and the bounded content required for later context. They do not contain raw provider JSON, host paths, internal temporary names, Rust debug strings, or filesystem error text.

Complete tool calls and results are included in later provider context in canonical order. Provider call identifiers and live reasoning continuation may remain bounded temporary run state; a new run can use deterministic server-generated call identifiers derived from canonical tool-call identities. Restart never depends on provider-hosted conversation state.

A provider response with no tool call succeeds only when it contains one valid complete final assistant answer under the existing terminal rules. A tool-requesting response cannot also terminate the run with a final answer.

### Tool-operation durability and recovery

Every tool call has prepared, dispatched, and terminal operation facts. Read-only calls commit prepared and dispatched state around the observation even though they have no workspace effect. Mutating calls use the filesystem publication boundary described above and never hold a SQLite transaction while reading, writing, hashing, synchronizing, or renaming files.

A normal tool result commits only after trusted code validates the operation outcome. The result is supplied to the provider only after that commit. A provider turn is never dispatched from an in-memory-only tool result.

Startup never resumes a provider loop or repeats a tool call. It reconciles each incomplete operation before accepting new work:

- a committed call without dispatch receives a not-dispatched result;
- an incomplete read-only call receives an interrupted result without rereading the worktree;
- a mutating operation whose exact target, parent identity, operation staging state, and before-or-after digest prove a completed or not-applied outcome receives that proven result;
- an exact unpublished operation temporary may be removed only when the target is proven unchanged and the temporary identity and digest match the committed operation;
- mismatched target identity, unexpected publication, ambiguous temporary state, unknown digest, link, reparse point, or out-of-scope state remains uncertain and is not modified automatically; and
- the top-level run terminates as interrupted when all workspace effects are known, or uncertain when any effect cannot be proven.

Recovery inspection is confined to the expected mutable worktree and exact operation-bound temporary identity. It never reads the original import source, baseline content for mutation authority, another workspace, or a path supplied by an error or client. It never publishes an unapplied intended edit during recovery.

A tool call committed before a crash always receives a durable terminal tool result or an uncertainty result during recovery, so later context does not contain an unmatched callable effect. Recovery results state only what the server can prove and never convert missing evidence into success.

### Cancellation and uncertainty

Read-only loops check cancellation between bounded reads and traversal steps. A mutation may honor cancellation before dispatch. After dispatch, trusted code finishes or reconciles the short bounded publication critical section rather than abandoning a possibly applied write midway. The run remains active until controlled tool execution stops and terminal facts commit.

Cancellation after a proven tool outcome preserves the committed call and result and then terminates the run as cancelled. An unresolved mutating effect terminates the run as uncertain regardless of cancellation intent and creates the exact session uncertainty blocker defined by ADR 0005.

The terminal client exposes a sanitized blocker with run identity and built-in tool kind, never a host path, temporary name, content, digest, or raw error. Local-owner acknowledgement parks but does not resolve, reverse, retry, or erase the uncertain effect. New input remains blocked until that idempotent acknowledgement commits.

### Limits and presentation

The server enforces fixed versioned limits for provider turns, calls per turn, calls per run, mutations per run, path bytes and depth, input schemas, directory entries, file bytes, read output, search traversal, search matches, replacement count and bytes, cumulative tool-result bytes, context bytes, elapsed run time, storage growth, and event payloads. Every arithmetic conversion is checked and architecture-neutral.

Tool output is untrusted repository data. Provider serialization, persistence decoding, protocol DTO conversion, event delivery, and Ratatui rendering each reapply their own bounds. The terminal displays concise durable tool-call and result summaries through the existing terminal-safety boundary and never renders raw ANSI, control sequences, host paths, or unbounded file content.

The application protocol exposes only deliberate sanitized transcript and event DTOs for committed tool calls, results, run changes, and uncertainty state. It does not expose a client-callable file API, raw worktree endpoint, or temporary progress stream in this section.

### Validation

Implementation requires deterministic persistence, provider, real-filesystem, authenticated IPC, and terminal tests covering:

- exact tool schemas, fixed catalog admission, model capability checks, and rejection before dispatch of unknown, malformed, duplicate, contradictory, or excessive calls;
- canonical multi-turn ordering, provider usage accumulation, complete call/result pairing, deterministic context reconstruction, snapshot pagination, replay, and projection rebuild;
- root and nested listing, UTF-8 line windows, complete-file digests, literal search, truncation, binary handling, changing files, and cancellation;
- edit digest conflicts, ambiguous replacements, overlapping replacements, empty files, exclusive file and directory creation, private controls, executable-bit narrowing, and no source or baseline modification;
- absolute paths, traversal components, alternate streams, links, junctions, reparse points, special files, identity changes, case or normalization collisions, destination races, and cross-session identifiers;
- process termination before prepare, before dispatch, during staging, after synchronization, after publication, and before result commit, with recovery proving success, not-applied, interrupted, or uncertain without replay;
- uncertainty blocking and exact local-owner acknowledgement without claiming or changing the effect;
- fixed per-file, per-call, per-turn, per-run, time, output, context, storage, and subscriber limits;
- absence of provider credentials, IPC authentication material, absolute host paths, baseline data, source paths, raw provider JSON, and raw filesystem errors from tools, durable facts, audit facts, events, errors, logs, and terminal output; and
- native macOS, Linux, and Windows behavior on supported `x86_64` and `aarch64` targets.

## Consequences

- Morons becomes a coding agent that can inspect, search, create, and edit an imported isolated worktree without shell access.
- The provider receives repository content only through bounded committed tool results selected during an authorized run.
- Exact structured built-ins keep the trusted capability surface smaller than a generic filesystem or command endpoint.
- Durable call/result history makes model context and client replay independent of one provider stream or client connection.
- Digest preconditions and atomic publication prevent stale model edits from silently overwriting a different observed file.
- Crash recovery preserves proven file effects and blocks ambiguous mutations rather than repeating or inventing outcomes.
- Deletion, rename, Git, sandboxed commands, diff review, and controlled export remain separate capabilities that must compose with this workspace lease and durability boundary.
