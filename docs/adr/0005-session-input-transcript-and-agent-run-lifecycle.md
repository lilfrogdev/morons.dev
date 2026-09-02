# ADR 0005: Session input, transcript, and agent-run lifecycle

## Status

Accepted as amended by ADR 0012

## Context

A durable session needs a precise boundary between accepted user input, provider execution, tool effects, completed assistant output, transient streaming, cancellation, and recovery. Client disconnection and server restart must not make it ambiguous whether input was accepted or whether a run completed.

Multiple authenticated terminal clients may observe or mutate the same local session. The server therefore needs one authoritative ordering for session history and run state that does not depend on a connection, attachment, process, wall-clock timestamp, provider response identifier, or temporary runtime.

ADR 0002 establishes the server application and subscription boundary. ADR 0003 establishes append-only canonical history, one nonterminal top-level run per session, idempotent mutations, external-effect state, and restart recovery. ADR 0004 establishes OpenCode service, model, credential, and Responses protocol boundaries. This decision defines how those boundaries compose for session interaction and agent execution.

## Decision

### Local-owner input and attribution

An authenticated local client submits session input as a server application mutation containing:

- a stable mutation request identifier;
- the target session identifier;
- non-empty bounded UTF-8 user text;
- the selected OpenCode service; and
- the exact reviewed model identifier.

Every submission selects its service and model explicitly. The server validates the combination against the reviewed built-in manifest and records it on the run. Session history and execution do not depend on client-local model state.

The durable actor for direct user input is `LocalOwner`. It identifies the current operating-system owner authority, not a particular terminal process, connection, attachment, or person. Actor attribution is a canonical audit and transcript fact and is never authorization evidence by itself.

A user-message identifier and run identifier are independent server-generated opaque 128-bit values. They are not derived from the mutation identifier, session identifier, provider identifier, sequence, timestamp, or filesystem state.

### Atomic acceptance

The application service first returns any committed exact mutation result. For a new candidate mutation, it reserves a global run permit before entering one SQLite transaction that:

1. resolves the mutation result again to close concurrent retry races;
2. validates session lifecycle and authorization;
3. verifies that the session has no nonterminal top-level run or unacknowledged uncertainty blocker;
4. verifies all input, storage, context, model, credential-generation, and budget limits;
5. appends the immutable attributed user-message fact;
6. appends the run-accepted fact with its selected service, model, protocol revision, credential generation, context-policy version, and limits;
7. updates transcript, active-run, session, and idempotency projections;
8. appends structured audit facts and ordered delivery events; and
9. commits before returning success or starting execution.

The accepted response returns the stable user-message and run locators and the committed run state. An exact retry returns the same result even when the run is active or terminal. Reuse of the mutation identifier with different text, session, service, model, or normalized options fails with a request conflict.

A rejected submission does not append a user message or create a run. Input is accepted only when the session has no nonterminal run; otherwise the server returns a structured busy result. Concurrent submissions are serialized by authoritative session state, so at most one can create the next run.

Global execution capacity is reserved before acceptance. Capacity exhaustion rejects the mutation with a structured resource-limit result rather than creating a durable queued run. Once accepted, the run is server-owned and client disconnection has no effect on its lifetime.

### Context construction

The server constructs provider context through a deterministic versioned policy from validated canonical session entries and an accepted compaction checkpoint when one is needed. Context includes attributed user messages, completed assistant messages, and complete durable tool calls and results in session-entry order. Run failures, audit facts, presentation strings, token deltas, raw provider data, and incomplete output are not model messages.

A user message remains canonical when its run fails, is cancelled, is interrupted, or becomes uncertain. A new run's context policy accounts for every prior canonical user message and completed result rather than silently deleting failed turns.

The server computes context limits before accepting input. It records the context-policy version and exact source-entry high water used for each provider operation. Model input is reconstructed from canonical facts; raw serialized provider requests are not authoritative state.

### Run states and supervision

A top-level run begins in `accepted`, moves once to `active`, and ends once in one terminal state:

- `succeeded`;
- `failed`;
- `cancelled`;
- `interrupted`; or
- `uncertain`.

Terminal state never reopens. Continuation always uses a new input mutation and run identifier. Accepted and active are both nonterminal and keep the session busy.

The server owns a bounded run supervisor keyed by run identifier. It holds the global execution permit, cancellation signal, provider task, and temporary Responses continuation state. A run task cannot outlive the supervisor or transfer ownership to a client connection.

The active transition commits before the first provider dispatch. Provider and tool operations use distinct durable operation identifiers and the prepared, dispatched, and outcome facts defined by ADR 0003. The run remains active across bounded provider turns and tool operations.

A successful run commits its complete final assistant message, provider outcome, usage, terminal run fact, projections, audit facts, and delivery events atomically. Success requires a validated complete assistant message. Temporary text, an incomplete stream, or a provider terminal event without a valid final assistant message cannot produce success.

### Provider turns and tool-loop ordering

Each provider turn uses the service, model, protocol revision, credential generation, context-policy version, and limits committed for the run. Credential mutations and provider dispatch checks are serialized so a changed credential generation prevents a new dispatch under the old run generation. A request that was already dispatched retains its recorded generation and outcome boundary.

Responses events are normalized into bounded provider-neutral items. Assistant text deltas remain ephemeral. Complete assistant text, complete tool calls, usage, and terminal provider status are validated before they become durable facts.

When a completed provider turn requests tools, the server:

1. validates every complete tool call against the run's server-owned tool catalog and capabilities;
2. assigns canonical tool-call identifiers and ordering;
3. atomically commits the provider outcome, bounded usage, any complete assistant text, and all validated tool-call facts before dispatch;
4. executes calls sequentially in canonical provider order;
5. records each prepared and dispatched tool operation before its effect;
6. validates and commits each bounded tool result before using it as model input; and
7. constructs the next provider turn from committed facts.

A partial tool call, undeclared tool, duplicate provider call identifier, malformed arguments, excess call count, or unsupported result fails the run without dispatching that call. Tool output remains untrusted after execution and cannot bypass provider-input, transcript, or output limits.

The server enforces fixed per-turn, per-run, and aggregate limits for provider calls, tool calls, context, output, time, and usage. Limit exhaustion stops controlled execution and produces a durable structured terminal outcome. Partial assistant output never becomes canonical history.

### Canonical transcript and run facts

Canonical session entries use one monotonic session-entry sequence and contain only complete versioned facts:

- an attributed user message bound to its run;
- a completed assistant message bound to its run and model selection;
- a validated tool call bound to its run and provider operation; and
- a validated tool result bound to its tool call and execution operation.

Run-state, cancellation, provider-operation, recovery, compaction, idempotency, and audit facts are canonical but are not presented as model-authored transcript messages. Provider errors and run failures use stable structured classifications with bounded non-secret metadata rather than raw provider bodies, headers, URLs, SDK errors, logs, or presentation strings.

Assistant attribution identifies the run, OpenCode service, and model that produced the message. A tool result is attributed to the trusted server tool operation. Neither kind can be submitted directly by an IPC client.

All canonical text and structured payloads have UTF-8 byte, collection, nesting, and item-count limits. Provider payloads are normalized into explicit versioned durable representations rather than serialized wholesale.

### Session snapshots and subscriptions

A session snapshot returns:

- the sanitized session summary;
- transcript entries through an immutable session-entry high water;
- the active run, run summaries referenced by the returned entries, and any unresolved uncertainty blocker as of the snapshot high water; and
- a session-event cursor read in the same transaction.

Transcript pagination fixes its entry and event high waters on the first page and uses opaque server-validated cursors. Every page derives transcript and run state only from facts at or below those high waters. Entries and run transitions committed afterward are delivered after the snapshot's session-event cursor. This gives snapshot pagination and event replay one gap-free boundary.

Each session has a scoped durable event stream ordered by the server's logical event sequence. Durable events project committed user messages, completed assistant messages, tool calls and results, run transitions, cancellation intent, uncertainty acknowledgement, recovery outcomes, and terminal failures. A transaction publishes its events only after commit and in committed sequence order.

A session subscription uses a dedicated authenticated connection. It validates session scope and the starting cursor, replays bounded pages from SQLite, then follows live commit notifications. Invalid, cross-session, or stale cursors fail with structured results. Per-subscriber queues and writes are bounded, and a slow subscriber is disconnected.

Assistant text deltas are ephemeral session events with a run identifier and a run-local monotonic delta sequence. They are emitted only after the durable active transition, are never stored or replayed, and may be dropped while a subscriber catches up. A client replaces displayed partial text with the complete durable assistant message. Losing every delta does not change transcript or run recovery.

### Cancellation

Cancellation is an idempotent application mutation containing a stable mutation request identifier, session identifier, and exact run identifier. It never targets whichever run happens to be current.

For a nonterminal run, the server commits cancellation intent and its audit and delivery facts before signaling the supervisor. The command returns the committed intent; terminal cancellation is delivered separately. The session remains busy until controlled provider and tool execution is known to have stopped and the terminal transaction commits.

An exact cancellation retry returns its prior result. A request for a terminal run returns its current terminal summary without reopening or changing it. A run becomes `cancelled` only after the supervisor proves its controlled execution stopped. An unresolved tool or workspace effect produces `uncertain`; provider usage uncertainty does not prevent cancellation after the local provider task stops. A stopped run with a known non-cancellation failure preserves that failure.

Client detachment, subscription loss, request timeout, and connection closure never imply cancellation.

### Uncertainty acknowledgement

An unresolved tool or workspace effect terminates the run as `uncertain` and blocks new session input. The session snapshot and durable event stream expose the blocker through sanitized operation kind and run identity without raw tool output or paths.

The local owner may acknowledge a blocker through an idempotent mutation containing a stable mutation request identifier, session identifier, and exact uncertain run identifier. The acknowledgement commits an attributed canonical fact, projection update, audit fact, and delivery event. It permits new input while preserving the run and external operation as uncertain; it never claims that the effect succeeded, failed, or was reversed.

An acknowledgement applies only to the current unresolved blocker. Exact retries return the committed result, and stale or cross-session run identifiers fail closed.

### Failure and recovery

Provider authentication, entitlement, rate-limit, availability, protocol, malformed-output, resource-limit, and internal failures map to stable sanitized run-failure classifications. A dispatched provider operation with no committed usable response remains uncertain for usage and billing, while the live run terminates as `failed` and restart recovery terminates it as `interrupted`. No assistant result is accepted. An unresolved workspace or tool effect terminates the run as `uncertain` and blocks input until acknowledged.

On graceful shutdown, the server stops accepting input, stops supervised execution, waits within a bounded deadline, and commits the terminal state it can prove. A run without prior local-owner cancellation becomes `interrupted`; an unresolved tool or workspace effect becomes `uncertain`. On startup, before accepting application operations, recovery applies ADR 0003 to every nonterminal run. Recovery publishes durable terminal and audit facts and performs no provider, tool, or filesystem effect.

A database commit failure or unknown commit outcome never publishes a durable event or successful command result. A complete provider response that cannot be committed does not become an assistant message and is not supplied to another provider turn.

### Validation

Implementation tests use real SQLite and authenticated IPC and cover:

- exact input retry, conflicting reuse, concurrent submission, busy-session rejection, and global-capacity rejection;
- actor, session, run, message, model, credential-generation, and context-policy bindings;
- atomic user-message and run acceptance with no execution before commit;
- deterministic context construction and snapshot pagination at a fixed high water;
- complete assistant commit, partial-stream loss, malformed Responses sequences, and bounded deltas;
- ordered tool calls and results with failures at every prepared, dispatched, and outcome seam;
- cancellation before dispatch, during streaming, during a tool effect, after termination, and across disconnect;
- restart recovery for accepted, active, cancelled, interrupted, and uncertain runs;
- uncertainty blocking, acknowledgement, exact retry, stale-run rejection, and preserved unresolved effects;
- gap-free session snapshot and replay, stale and cross-session cursors, multiple subscribers, and slow-consumer disconnection;
- sanitized failures and absence of credentials and raw provider payloads in persistence, events, errors, and logs; and
- per-session and global concurrency, context, output, event, time, call-count, and storage limits.

## Consequences

- A successful input response proves that both the user message and run identity are durable.
- One session has at most one nonterminal top-level run and no hidden input queue.
- Every completed transcript item has explicit actor, run, model, and operation provenance.
- Clients can detach, reconnect, replay committed history, and observe terminal outcomes without owning execution.
- Streaming remains responsive without making partial text authoritative or recoverable.
- Cancellation and restart preserve uncertainty instead of inventing completion or repeating effects.
- Explicit per-run model selection keeps execution deterministic across clients and restarts.
- The server remains authoritative for context, transcript order, tool dispatch, run state, limits, and publication.
