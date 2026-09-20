# ADR 0054: Durable steering messages

## Status

Accepted design; implementation started with an inactive schema foundation. Amends ADR 0005's input acceptance and
successful-run completion rules once implemented.

Schema 43 reserves per-session queue state and pending text records; no application
path writes them yet. Queues default to paused, target a run in the same session,
and hold at most sixteen 64-KiB messages (one MiB total). FIFO order uses enqueue
sequence, not reusable capacity slots. Actor 1 denotes `LocalOwner`. Mutation
history, attachments, storage-worker mutation operations, projections, integrity,
and delivery are still pending; this migration alone does not enable queueing.
Startup, cancellation intent, terminal run transitions, and archive preparation
pause existing queues without consuming messages; unarchiving does not resume
them. Session deletion removes queue records before
run facts, explicitly deleting pending text without touching the selected directory.

## Decision

The local owner can queue steering messages while a session's run is active.
There is no follow-up mode and no concurrent run. Enter submits normally when
idle and queues steering when busy; existing newline shortcuts are unchanged.

The server owns a bounded, durable per-session FIFO queue in SQLite. Queue
acceptance is distinct from transcript acceptance: pending messages are visible
as queued input, not canonical user messages or provider context. Client
connections do not own queue lifetime. Shell commands and context-control
commands are not steering messages.

### Authority and admission

Queue mutations require authenticated local-owner intent, stable mutation IDs,
explicit session identity, and expected queue revisions. Enqueue additionally
targets the exact active run; a stale target fails rather than starting a new
run. Edit and remove target exact queue-item identities and revisions. Exact
mutation retries return the original committed result; conflicting reuse fails.

Steering inherits the target run's model, service, credential generation, and
pinned project guidance. It cannot switch models, override policy, bypass an
uncertainty blocker, or relax context limits. Skill invocations and attachments
need the same server validation and bounded preparation as ordinary input.
Queue capacity must bound item count and aggregate text and attachment bytes,
with existing per-message limits retained. Rejection retains the client's draft.

Pending input and its attachments remain outside provider context and compaction
until delivery. Enqueue, edit, remove, pause, resume, and delivery commit their
attribution, idempotency results, projections, and ordered events together.
Snapshots and replay expose queue state through the existing gap-free event
cursor boundary. Pending items are terminal-sanitized like all other input.

### Delivery boundary

Consume at most one item before each provider turn, after all tools from the
preceding assistant turn have committed their results. Never interrupt an
in-flight provider request or tool batch to inject input. Delivery atomically
appends a `LocalOwner` user message bound to the existing run and marks the queue
item delivered. Startup integrity validation must recognize this explicit
provenance without weakening validation of ordinary run-initiating messages.

A completed final assistant response and queue state must be evaluated in the
same transaction. If eligible steering is pending, commit the response, deliver
one item, and keep the run active. Otherwise commit success. A concurrent enqueue
therefore either precedes finalization and is consumed, or is rejected as stale;
it must never silently become a follow-up run.

Rebuild provider context from committed history after delivery. Preserve required
opaque tool-call and reasoning provenance; do not indiscriminately clear provider
continuation state. Context accounting, checkpoint source boundaries, data-use
policy, and credential-generation checks still apply before dispatch. If an item
cannot be delivered safely, retain it, pause delivery, and expose the bounded
failure rather than dropping it or repeatedly attempting dispatch.

### Pause, recovery, and owner controls

Cancellation intent pauses pending delivery before further steering can be
consumed. Failure, interruption, uncertainty, and server restart also leave the
remaining queue paused. Neither reconnect nor uncertainty acknowledgement resumes
it. Recovery does not dispatch work or replay delivered messages.

The owner can inspect, edit, remove, and explicitly resume pending messages.
Resume against an active run binds to that exact run. When idle, explicit resume
must use ordinary new-run admission for the first item, atomically recording its
delivery; remaining items steer that run. This is owner-authorized resumption,
not automatic follow-up execution. Terminal runs never reopen. Existing capacity,
model selection, credential, archival, and uncertainty guards remain in force.

Archive pauses delivery; deleting a session removes its queue and managed
attachments without touching its selected working directory. Attachment cleanup
must respect references from both pending items and delivered transcript entries.

### CLI

Show pending messages and paused state separately from transcript entries, with
controls to retrieve/edit, remove, and resume. Do not clear a submitted draft
until durable acceptance is confirmed. Unknown mutation outcomes are resolved by
identity, not a new submission. Editing must not overwrite newer local drafts;
concurrent delivery or edits return a conflict and refresh the queue.

## Implementation sequence

1. Add bounded queue records, migration, mutation history, projections, recovery,
   and integrity validation through the existing storage worker.
2. Add versioned protocol/application mutations and snapshot/event DTOs with
   idempotency and concurrent-client tests.
3. Integrate transactional consumption at tool and final-response boundaries,
   including paused recovery and explicit resume admission.
4. Add CLI enqueue and queue controls without changing newline or image-marker
   editing behavior.

## Required verification

- FIFO, one item per turn, and no delivery during an in-flight operation.
- Enqueue versus finalization, cancellation, edit, removal, and archive races.
- Exact retry, conflicting mutation reuse, stale run/revision, and cross-session IDs.
- Queue count/byte limits, image validation/storage, skills, and context exhaustion.
- Persisted pause on failure, restart, and cancellation; no automatic execution.
- Explicit resume with model admission, uncertainty and capacity guards preserved.
- Canonical attribution, provider continuation, compaction, and startup integrity.
- Gap-free snapshots/replay, multiple clients, disconnect, and unknown commit outcomes.
- CLI draft retention, Unicode/image editing, queue presentation, and shortcut tests.
- Rustfmt, warnings-denied Clippy, relevant workspace tests, and manual terminal QA.
