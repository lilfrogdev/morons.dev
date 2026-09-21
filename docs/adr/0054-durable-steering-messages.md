# ADR 0054: Durable steering messages

## Status

Accepted design; implementation started with an inactive schema foundation. Amends ADR 0005's input acceptance and
successful-run completion rules once implemented.

Schema 43 reserves per-session queue state and pending text records. Queues default to paused, target a run in the same session,
and hold at most sixteen 64-KiB messages (one MiB total). FIFO order uses enqueue
sequence, not reusable capacity slots. Actor 1 denotes `LocalOwner`. This migration
alone does not enable queueing; the protocol milestone below exposes storage acceptance only.
Startup, cancellation intent, terminal run transitions, and archive preparation
pause existing queues without consuming messages; unarchiving does not resume
them. Session deletion removes queue records before
run facts, explicitly deleting pending text without touching the selected directory.

Schema 44 adds ordered mutation facts and receipts and reserves a global mutation
operation kind. Facts retain accepted text, operation kind, and explicit run targets;
startup repairs mismatched pending text, item revisions, and FIFO provenance from
validated canonical history.
Startup also validates canonical text and recomputes each mutation fingerprint from
its session, prior queue/item revisions, operation, and payload, including superseded edits.
A queue's revision-one mutation must be enqueue, never pause or resume, even when
its fingerprint and projected state agree. Item histories must start with one
enqueue, advance edits by one revision, and allow removal only at the current
revision, with no subsequent reuse of the item.
Queue targets must match the latest enqueue/resume target. At an explicit pause,
resume, or initial enqueue revision, the projected pause state must match that fact.
Schema 44 also records lifecycle pauses with ordered sequences, exact targets,
resulting revisions, and cancellation, terminal-transition, archive, or recovery
reasons. Non-recovery pauses bind their originating durable fact and timestamp;
pause facts and queue updates commit together. Recovery pauses commit before run
recovery and never consume or resume pending input. Startup validates source
bindings, combined mutation/lifecycle revision order, and the latest pause state.
Historical cancellation, terminal, and archive sources require a corresponding
pause when the preceding known queue state was active and targeted that source.
Enqueue and resume histories must target an already accepted run without an earlier
applied cancellation or terminal transition; mutations cannot occur while archived,
and only explicit resume can retarget an existing queue. Archive preparation rejects
new mutations immediately, before the archived session projection is finalized;
exact retries still return their original receipt. For queues with a recorded
initial mutation, every pending item must have a canonical enqueue fact.
Historical occupancy is checked at every mutation boundary, not only at the final
queue state. After validation, queues with complete mutation history are rebuilt
transactionally from canonical mutation and lifecycle facts, including edited text,
FIFO provenance, removals, explicit targets, and pause state. Capacity slots are
projection details and may be reassigned without changing FIFO order.
Legacy foundation-only rows remain readable and are not reconstructed. Startup
validates canonical history independently of projections, reconstructs missing or
conflicting history-backed projections, and validates the result before committing.
Regression coverage checks repeated startup repair, canonical corruption rejection,
recovery pausing once, and transactional reconstruction rollback.
Database startup migrates and validates schema and quick integrity before canonical
validation and projection reconstruction. Steering postvalidation runs inside the
rebuild transaction; the separate final database integrity check runs after commit.
Backend recovery starts only after database open succeeds, and pauses queues before
recovering nonterminal runs. These are separate commit boundaries, not an atomic
whole-startup rollback guarantee.
The storage-worker mutation core exercises enqueue, edit, remove, pause,
and exact-active-run resume with queue/item revision checks and durable retry
results. The protocol milestone exposes these text-only operations with gap-free
snapshots/replay; prepared skills/attachments and idle-resume admission remain
prerequisites for delivery. No UI or delivery path is enabled.

### Protocol admission milestone

Authenticated local-owner IPC may mutate text-only queues, read transactional
snapshots and bounded replay pages, and subscribe using a separate session-bound
steering cursor. This exposes durable storage acceptance only, not transcript or
provider delivery. Resume requires an exact active run; idle resume, attachment
preparation, skill expansion, provider delivery, and CLI queue controls remain
unavailable. Text is retained literally until a future delivery admission validates
it. No request can submit attribution, delivery facts, or provider outcomes.
Subscriptions register commit notifications before reading history, replay durable
facts in bounded pages, and disconnect slow writers using existing transport limits.
Notifications are wakeups, not authoritative events; reconnect resumes from the last
received steering cursor. Session deletion ends the subscription. Exact retries
resolve unknown mutation acknowledgments without repeating effects.

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
Snapshots, replay, and notifications must expose queue state through a consistent,
gap-free cursor boundary. Steering uses a separate session-bound cursor and
subscription, not transcript events. Pending items
are terminal-sanitized like all other input.

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

### Storage snapshot and replay prototype

Worker requests read queue state and its canonical steering high-water
in one SQLite read transaction. A separate session-bound cursor replays bounded
change notices from mutation and lifecycle facts, not transcript delivery events.
Notices invalidate queue state; clients must refresh a snapshot rather than treat
notices as historical message payloads. Paginated readers advance to the last
returned notice, not the page high-water, until caught up. Restart pause facts
participate in replay; reconnect and reads never resume or consume input.
Protocol subscriptions replay the same durable notices through authenticated IPC.
Steering facts do not change session-list ordering or its event cursor; queue
consumers must use the separate steering cursor. Any production integration must
keep snapshot, replay, and notification boundaries consistent.

Storage regression coverage includes competing requests through the serialized
worker, independent paginated readers across populated sessions, repeated startup
repair, legacy-row preservation, and repair-transaction rollback on execution or
post-rebuild validation failure. Connection regressions additionally cover unknown
acknowledgments, exact retries, stale revisions, snapshot/subscription boundaries,
live wakeups, bounded multi-page reconnect replay, session isolation, idle-session
deletion termination, and slow-writer disconnection. All clients still
share the single authoritative storage worker.

## Implementation sequence

1. Add bounded queue records, migration, mutation history, projections, recovery,
   and integrity validation through the existing storage worker.
2. Add versioned protocol/application mutations and snapshot/event DTOs with
   idempotency and concurrent-client tests.
3. Integrate transactional consumption at tool and final-response boundaries,
   including paused recovery and explicit resume admission.
4. Add CLI enqueue and queue controls without changing newline or image-marker
   editing behavior.

### Delivery implementation contract

Delivery must add canonical provenance before enabling consumption. Current startup
validation requires exactly one user entry per run, and queue reconstruction knows
only mutation and lifecycle facts; simply inserting a user entry and deleting a
pending row would violate both invariants on restart.

- Record a delivery fact binding the session, exact run, item and item revision,
  enqueue sequence, queue revision, transcript message, and committed boundary.
  Validate it against the canonical edited text and FIFO state, not projections.
- Preserve the unique ordinary run-initiating message and its retry fingerprint.
  Additional user entries require explicit delivery provenance; do not relax the
  existing count check without validating every additional entry.
- Include delivery in sequence uniqueness, queue reconstruction, snapshot/replay,
  post-commit notifications, session deletion, and startup integrity checks.
- Prepare skills with ordinary input validation before consumption, outside the
  storage transaction. Recheck the exact item/queue revision and active run when
  committing; a concurrent edit, pause, cancellation, or archive invalidates the
  preparation rather than dispatching stale input. Pending text is not implicitly
  approved as literal provider input merely because storage admission accepted it.
- Share consumption eligibility between the between-turn boundary and final
  response transaction. A final-response delivery must not consume a second item
  at the next loop iteration, including after compaction. Never consume while a
  provider operation or any tool from the preceding turn is unresolved.
- Preserve provider continuation provenance when rebuilding committed context.
  Recovery validates delivery but never redispatches it. Unsafe preparation or
  admission retains the item and pauses the queue with a bounded visible reason.
- Keep idle resume on ordinary new-run admission, including policy, credential,
  uncertainty, and prepared-input guards; never reopen a terminal run.

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
