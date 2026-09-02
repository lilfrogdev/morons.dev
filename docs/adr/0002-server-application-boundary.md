# ADR 0002: Server application boundary and transport posture

## Status

Accepted as amended by ADR 0012

## Context

The server is authoritative for sessions, agent runs, tools, credentials, persistence, and future execution capabilities. Local IPC now authenticates both peers before the application protocol begins, but transport authentication does not define application authorization, resource ownership, retry behavior, event delivery, or durable state.

Session persistence and recovery will introduce identifiers, commands, queries, and events that must remain coherent across the server, protocol, and client. Coupling application behavior directly to IPC handlers or database records would make transport changes and schema migrations unsafe. Publishing a network API before these semantics stabilize would also create an early compatibility commitment and expand the security boundary without a current requirement.

The design should retain the useful properties of a headless programmable server without adding a network listener, HTTP framework, OpenAPI generation, or SDK pipeline before they are needed.

## Decision

The server application boundary will consist of concrete commands, queries, and event subscriptions. Transport adapters authenticate a peer, establish request context, strictly decode bounded protocol data, invoke application services, and encode results. They do not own business authorization, persistence transactions, provider access, sandbox access, or side-effect policy.

### Session host model

One authoritative server manages many independently resumable sessions. Session identity and lifetime are independent of the server process, transport endpoint, and client connections. An authorized client may list sessions and attach to, detach from, or switch between them without transferring ownership or implicitly stopping active work. Disconnecting a client does not cancel a run; cancellation requires an explicit authorized command.

A session is durable conversation and workspace state. A run is a bounded execution within that session. A runtime, subprocess, or Python kernel is temporary execution state and is not the authoritative session record. Inactive sessions require no live runtime, while multiple sessions may run concurrently within server-enforced global and per-session limits.

Each session has an isolated mutable workspace and execution context. A session permits at most one nonterminal top-level agent run and serializes mutations to authoritative session state. Queries and subscriptions may coexist with that run. ADR 0005 defines input acceptance, run execution, transcript snapshots, cancellation, and session subscriptions.

Subagents, agent teams, and workflows are outside this decision. Future decisions may add separately scoped execution and coordination resources, but they must preserve session isolation, explicit capabilities, bounded concurrency, and server authority.

### Application services

Application services will:

- validate resource identifiers and operation inputs;
- authorize every operation against server-owned state and explicit capabilities;
- enforce session, project, concurrency, time, output, and budget limits;
- coordinate persistence transactions and external side effects;
- provide idempotency for mutations that can be retried;
- produce structured audit events without credentials or authentication material; and
- return deliberate protocol DTOs rather than persistence records.

The first implementation will use concrete Rust services and handlers. It will not introduce a generic transport framework, plugin system, or speculative service abstraction.

### Commands and queries

Commands request state changes or external effects. A command that may be retried after a disconnect must carry a stable request identifier within its authorization scope. The server records enough information to return the prior outcome or report an uncertain result without repeating an external side effect blindly.

Queries are side-effect free from the client's perspective. List operations use stable ordering and opaque cursors rather than database offsets when concurrent changes could otherwise duplicate or omit results.

Application-owned resource identifiers are opaque, server-generated, stable across restarts, and independent of database row identifiers, filesystem paths, process identifiers, provider identifiers, and transport connections. Possession of an identifier is never authorization.

### Event subscriptions

Application events exposed to clients are scoped, typed, sanitized projections. They are not raw database rows, internal event facts, logs, model-provider payloads, or sandbox output.

Durable authoritative events include accepted user messages, completed assistant messages, durable tool results, run-state transitions, compaction records, and terminal outcomes. Each resumable durable event stream has an ordered cursor defined by server state. Snapshot and subscription operations must compose without a gap in which committed changes can be lost. Invalid, stale, unauthorized, or cross-scope cursors fail closed.

Token deltas, heartbeats, and temporary progress updates are ephemeral presentation events. They may be dropped or coalesced, are not replayable, and must never be required to reconstruct authoritative state. After reconnecting, a client obtains a durable snapshot or resumes from a durable cursor rather than attempting to recover missed ephemeral events.

Per-subscriber buffering is bounded. A slow subscriber is disconnected rather than allowed to consume unbounded server memory, and the client resumes from the most recent durable cursor it received.

The persistence ADR will define the exact durable event vocabulary, event storage, cursor retention, compaction, and recovery semantics before subscriptions are implemented.

### Current transport

Authenticated local IPC is the only current application transport. The existing operating-system authorization, randomized endpoint registration, mutual HMAC proof, framing limits, and protocol handshake remain the local connection boundary.

Application operations are independent of IPC framing, but this does not require a second wire protocol. The local client will use typed request, response, and event DTOs over the existing authenticated connection.

The authenticated local IPC surface consists of typed application operations. ADR 0004 adds narrow local-owner operations to configure, replace, remove, and inspect non-secret credential status. Subsystem access remains mediated by server application services.

A future network API must be introduced by a separate architecture decision and threat-model update. It must define TLS termination, authentication, authorization, tenant isolation, origin policy, request limits, rate limits, event replay, deployment topology, and compatibility guarantees. Loopback binding alone is not authentication and is not equivalent to owner-controlled local IPC.

### Contract evolution

Protocol DTOs and error variants are deliberate compatibility contracts. They use strict decoding, explicit versioning, bounded fields, and structured errors. Presentation strings remain client-owned.

Persistence schemas and internal durable facts may evolve through migrations without becoming public contracts. Adapters may map the same application operation to different future transports, but all adapters must preserve the same authorization, idempotency, limits, and audit semantics.

## Consequences

- Local operation remains fast and small because no network stack or API-generation pipeline is added.
- Application authorization cannot be bypassed by adding or changing a transport adapter.
- Durable storage can evolve without exposing its schema to clients.
- Stable identifiers, idempotent mutations, cursor pagination, and resumable events are designed before session implementation.
- One server can host concurrent independent sessions without making their lifetime depend on client attachments.
- Durable events remain recoverable while high-frequency presentation updates may remain ephemeral.
- A future HTTP or SDK surface can reuse application services without duplicating privileged logic.
- Transport adapters require explicit DTO mapping rather than returning internal records directly.
- Network integrations remain unavailable until their security and compatibility costs are justified and reviewed.

## Alternatives rejected

- A public HTTP API now would expand the attack surface and freeze immature contracts without a current second-client or remote-access requirement.
- Replacing authenticated local IPC with loopback HTTP would weaken the established local transport boundary.
- Putting business logic in IPC handlers would couple authorization and persistence to one transport.
- Exposing database records would turn storage migrations into protocol changes and risk leaking internal or sensitive fields.
- Treating resource identifiers as capabilities would permit unauthorized cross-resource access.
- Retrying mutations without stable request identity could duplicate external side effects.
- A live-only or unbounded event bus would lose events across disconnects and permit slow clients to exhaust memory.
- One server per session would duplicate trusted control-plane state, complicate discovery and recovery, and make cross-session resource limits harder to enforce.
- Binding session lifetime to a client connection would make background execution and recovery unreliable.
- Treating ephemeral progress as durable history would increase storage and replay costs without adding authoritative state.
- A generic transport or privileged endpoint framework would add speculative complexity and weaken explicit capability boundaries.
