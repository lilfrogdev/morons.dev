# ADR 0003: Durable session persistence and recovery

## Status

Accepted as amended by ADR 0012

## Context

The server must manage many independently resumable sessions whose lifetime exceeds any client connection, server process, agent run, subprocess, or Python kernel. A client must be able to create, list, attach to, detach from, and resume sessions without owning their lifetime. The server must recover durable history after a crash without inventing completed work or repeating an external side effect blindly.

Session history includes untrusted user and model content, tool calls and results, run transitions, context compaction, and terminal outcomes. Session workspaces are mutable filesystem state and cannot be committed atomically with a database transaction. Provider calls, tools, subprocesses, and filesystem mutations are external effects even when initiated locally.

An in-memory store would not survive restart. Independent JSON or JSONL files would make cross-session listing, idempotency, transactional projections, migrations, and gap-free subscriptions harder to implement safely. A client-visible database or schema would bypass the application boundary established by ADR 0002.

The project will select its final authoritative persistence backend and recovery semantics before implementing sessions. It will not ship a temporary session format and migrate immediately afterward.

## Decision

### Authority and storage layout

SQLite is the authoritative local store for session metadata, immutable history, run state, idempotency records, internal durable events, delivery-event projections, compaction checkpoints, and structured audit facts. Repository working trees remain filesystem state owned by session workspaces; executable runtimes and kernel memory are temporary and never authoritative.

The database resides in a private data directory beneath the platform application root, separate from the control directory and runtime endpoint directory. Reinitializing local IPC authentication must not remove session data. Session workspaces use a separate private workspace root and server-generated workspace identifiers. Protocol DTOs never expose database paths, workspace paths, table keys, or SQLite row identifiers.

Only the server process holding the control root's lifetime host lock may open the authoritative database. Clients, sandboxes, subprocesses, tools, and kernels never receive database access. The database must be on a local filesystem and must not be opened from a network share or untrusted workspace.

Session content is sensitive, but the database is not a credential store. Server-managed provider, GitHub, and local IPC credentials remain outside it. Application-level database encryption is not introduced; confidentiality at rest depends on owner-only operating-system controls and storage encryption.

### SQLite profile

The server uses one reviewed, pinned Rust SQLite binding with one bundled, pinned SQLite implementation on every supported platform. Exactly one SQLite implementation may be linked into the server.

A dedicated server-owned storage worker serializes database access through a bounded request queue. It owns the sole authoritative connection and never holds a transaction open while awaiting provider, tool, subprocess, filesystem, or client activity. An online backup may open only its isolated destination connection inside that worker. Database records and SQL types remain private to the persistence module.

The connection uses and verifies these settings before application operations begin:

- rollback journaling with `journal_mode=DELETE`;
- `synchronous=EXTRA` for durable commits;
- full filesystem synchronization enabled where the platform supports it;
- foreign-key enforcement enabled;
- trusted-schema behavior disabled;
- defensive mode enabled;
- extension loading disabled; and
- explicit SQLite length, page-count, and other applicable resource limits.

The database has a fixed application identifier and an explicit schema version. The server uses parameterized, static SQL and does not accept SQL, table names, pragmas, database paths, or extension names from protocol or repository input.

### Durable records and projections

Server-generated session, run, entry, operation, checkpoint, and event identifiers are resource-specific opaque cryptographically random 128-bit values. Client-generated mutation request identifiers must also be cryptographically random 128-bit values. The store checks uniqueness, and collision or conflicting reuse fails closed. Durable logical sequences, rather than timestamps or identifiers, define creation order, session-entry order, and event order. Sequence exhaustion fails closed.

The canonical record model contains:

- a durable session identity, lifecycle, display metadata, and workspace identity;
- immutable ordered session entries for accepted user messages, completed assistant messages, durable tool calls and results, and other context-bearing facts;
- immutable run events from acceptance through one terminal outcome;
- durable external-operation records for prepared, dispatched, completed, failed, or uncertain effects;
- scoped idempotency records binding a request identifier to an operation fingerprint and prior outcome;
- immutable structured audit facts without server-managed credentials or authentication material; and
- accepted compaction checkpoints with explicit source coverage.

Mutable session-list summaries, current run status, latest compaction lookup, list ordering, and delivery-event rows are transactional projections. A projection never overrides a canonical fact and must be rebuildable from canonical records. Canonical facts and affected projections commit in the same SQLite transaction.

Persistent payloads use explicit versioned representations with strict decoding and bounded fields. Rust enum layouts, debug strings, provider payloads, protocol DTOs, and presentation strings are not persistence formats.

### Transaction and publication boundary

A successful database-only command atomically commits its canonical facts, projections, idempotency outcome, delivery events, and audit facts. Authorization checks that depend on mutable ownership or lifecycle state occur within the same transaction as the mutation.

Durable events and command success are published to clients only after the transaction commits. A commit that fails or has an unknown outcome is never reported as successful. Client disconnection does not roll back an already accepted command or committed run.

External effects use a durable intent and outcome boundary:

1. validate and authorize the operation;
2. commit the operation identity, normalized fingerprint, and prepared state;
3. commit a dispatched fact immediately before invoking the external effect;
4. perform the effect without holding a database transaction;
5. commit the validated outcome and resulting session facts; and
6. publish the durable result only after the outcome transaction commits.

An operation with no dispatched fact is definitely not dispatched. A dispatched operation without a committed outcome is uncertain. The server does not automatically execute an uncertain operation again. Tool-specific reconciliation may later prove an outcome, but absence of evidence is never treated as proof of failure or success.

### Idempotency

Every retriable mutation carries a stable request identifier within its authorization scope. The first accepted request stores a normalized operation fingerprint and durable outcome or resource locator. An exact retry returns the prior result. Reuse of the identifier with a different scope, operation, or fingerprint fails with a structured conflict.

Idempotency records for resource creation and external effects remain at least as long as the resource or effect history they protect. Pruning may replace a full result with a compact tombstone but must not make an old request identifier appear unused. Request fingerprints exclude server-managed credentials and local authentication material.

### Run lifecycle and restart recovery

A run is accepted, active, and then terminal. Terminal outcomes are succeeded, failed, cancelled, interrupted, or uncertain. Terminal state never reopens; continuation creates a new run identity. A cancellation request is durable intent, and a run becomes cancelled only after the server establishes that its controlled execution has stopped; an unprovable outcome remains interrupted or uncertain. ADR 0005 defines the exact input, state, transcript, and cancellation contract.

Accepted user input and the new run identity commit before model execution begins. Completed assistant messages commit only after a complete provider result is validated. Partial model text and temporary progress remain ephemeral. Durable tool calls commit before dispatch, and durable tool results commit before they may be supplied to another model request.

On startup, before accepting application operations, the server scans every nonterminal run and performs one idempotent recovery transaction per run. Recovery:

- appends an interrupted or uncertain terminal fact;
- updates the run and session projections;
- classifies external operations from committed prepared, dispatched, and outcome facts;
- preserves inspectable history;
- emits durable recovery and audit events; and
- performs no provider or tool side effect.

The initial implementation never automatically continues a run after server restart. An authorized user may start a new run only after unresolved operations and workspace state are safe or explicitly parked. Restart recovery repairs durable state; it does not revive an old execution or infer success from missing records.

### Workspace lifecycle

The database stores a server-generated workspace identity and lifecycle, not a client-selected authoritative path. Workspace directories are confined beneath the private workspace root and carry non-secret identity metadata. Existing paths, ownership, access controls, links, and identity metadata are verified before use.

Workspace provisioning and deletion are recoverable multi-step operations because SQLite and filesystem changes cannot share one atomic commit. The server first commits intent, performs only identity-bound filesystem operations beneath the expected root, and then commits completion. Startup recovery resumes or parks incomplete work from durable intent; it never deletes an arbitrary path supplied by a client or malformed database record.

A completed database fact is not proof that arbitrary subprocess writes reached stable storage or that an externally modified workspace still matches history. After abnormal termination, uncertain workspace mutations or background processes prevent automatic continuation. Built-in file mutation tools must establish their own durable write and reconciliation contract before recording success.

Each active runtime receives access only to its session workspace and explicit capabilities. It receives neither the database and backup roots nor another session's workspace.

### Durable subscriptions

Every committed delivery event receives a monotonic logical event sequence. Delivery rows are typed internal projections and are mapped to sanitized protocol event DTOs. They do not store protocol wire objects as canonical facts.

A snapshot is read with a durable high-water sequence. Subscription replay begins strictly after that high water, so commits between snapshot creation and live attachment are recovered from storage. The server applies scope and authorization checks to the snapshot, cursor, replay query, and live stream.

Delivery events have bounded retention by age and storage quota. A cursor older than the retained low water fails with a structured stale-cursor result that requires a new snapshot. Pruning delivery rows never deletes canonical session history. Ephemeral token deltas, heartbeats, and progress updates have no durable sequence and are never replayed.

### Context compaction

Context compaction is a durable lossy projection, not deletion or rewriting of session history. A checkpoint records its session, source entry count and high water, source digest, compaction policy version, accepted summary, token estimate, and lineage to any previous checkpoint.

The server validates that a checkpoint covers an exact ordered source prefix before using it. The model may generate summary content, but trusted server code chooses the covered prefix, computes its digest, validates the checkpoint, and selects the uncompacted tail. An invalid or incompatible checkpoint never replaces its source; the server uses an earlier valid checkpoint or a bounded source-derived projection and fails if no legal context fits.

Original entries remain available for recovery, audit, export, and future projections. Database vacuuming, event-delivery retention, and context compaction are separate operations.

### Migrations and corruption

Schema creation and forward migrations run under the lifetime host lock before the endpoint accepts application operations. Each migration is ordered, transactional, and advances the schema version only in the committing transaction. A database with the wrong application identifier, a newer schema, a failed integrity check, invalid canonical facts, or an incomplete unsupported migration fails closed.

The server never silently deletes, recreates, downgrades, or falls back from an existing authoritative database. Projection damage is repaired only from validated canonical records. Canonical corruption preserves the original files for diagnosis and requires an explicit verified restore or migration path.

Before a migration that can rewrite or remove existing information, the server creates and verifies a consistent database backup. A migration failure leaves the prior database authoritative.

### Limits, retention, and deletion

The server enforces fixed or configured limits for database size, session count, entry count, payload size, tool-result size, event backlog, idempotency records, and workspace usage. It rejects new work with a structured resource-limit error before uncontrolled growth. It never silently discards canonical history to regain space.

Archiving a session stops active work but retains history and workspace state. Explicit deletion is an idempotent lifecycle with a durable tombstone, runtime shutdown, confined workspace cleanup, and database cleanup. Recovery can continue an interrupted deletion without reactivating the session. Deletion is not represented as successful until required cleanup reaches its defined terminal state.

Logical deletion and SQLite page reuse are not guarantees of forensic erasure. Filesystem encryption, storage-device behavior, backups, and privileged administrators remain outside that guarantee.

### Backup and restore

A live database backup uses SQLite's online backup API and produces a verified consistent database snapshot. Copying the live database file directly is unsupported. Backup files receive the same owner-only controls as the authoritative data and must not contain server-managed credentials or local IPC authentication material.

A database backup protects session metadata and history but is not described as a complete session backup because mutable workspace contents are separate. A future full-session export or backup must quiesce the session and bind a workspace snapshot to an exact database high water and manifest.

Restore is an explicit offline operation with the server stopped. It validates ownership, file type, application identifier, schema support, integrity, and workspace references before atomically installing restored state. Automatic fallback to the newest available backup is not allowed.

### Implementation and validation

Persistence belongs to a concrete server-owned module rather than the protocol crate. The module exposes typed transaction operations and domain results, not raw SQL connections or generic query execution.

Tests use real SQLite and real filesystem boundaries. They cover transaction rollback, duplicate requests, request-identifier conflicts, process termination at durability seams, nonterminal-run repair, uncertain external effects, projection rebuilds, stale cursors, compaction coverage, migration fixtures, newer schemas, corruption, quotas, constrained workspace recovery, online backup, restore, and Unix and Windows access controls.

## Consequences

- SQLite provides cross-platform transactions, integrity constraints, migrations, and backup without adding a separate database service.
- One bounded storage worker keeps the implementation small and makes write ordering explicit.
- Session history, run recovery, idempotency, and subscription replay share one transactional authority.
- Server restarts preserve sessions but do not silently repeat interrupted work.
- Context compaction reduces model context without destroying evidence.
- Filesystem workspaces require explicit intent, identity, and reconciliation because they cannot be atomic with SQLite.
- Strong durability settings trade some write throughput for crash and power-loss safety.
- This decision does not define automatic run continuation, workspace snapshots, session branching, or multi-user authorization.

## Alternatives rejected

- In-memory persistence would make client disconnection and server restart destructive.
- JSON or JSONL per session would not provide atomic cross-record updates, durable idempotency, efficient global listing, or gap-free multi-session subscriptions without rebuilding database behavior.
- A client-visible SQLite file would bypass server authorization and couple clients to migrations and internal records.
- A remote database would add deployment, credentials, networking, and availability costs to a local-first single-host application.
- Full event sourcing of every internal implementation detail would add complexity without improving the required authoritative history.
- Mutable history rows would make compaction, audit, recovery, and uncertain-effect handling harder to verify.
- WAL mode would add checkpoint and sidecar-file lifecycle complexity without a concurrency benefit while the server intentionally owns one serialized connection.
- Holding a database transaction open during an external effect would increase lock time without making the effect atomic.
- Automatically retrying a dispatched operation without a committed outcome could duplicate an external side effect.
- Treating a compaction summary as replacement history would make a lossy model output authoritative.
- Copying a live SQLite file or automatically restoring a backup could produce inconsistent or surprising state.
- Automatically deleting old canonical history at a quota boundary would trade availability pressure for silent data loss.
