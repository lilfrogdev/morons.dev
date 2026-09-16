# Context budgeting and continuation

Status: child in-run compaction is implemented. It does not provide crash resume or replay.

## Child journal and in-run compaction contract

SQLite42 adds an append-only child journal under the existing durable task model
binding. Child indices remain 1–3. Provider and tool dispatches are recorded before
execution, and their results before use or publication. Each entry binds its
predecessor digest; checkpoints bind the journal head they summarize. Interrupted
children become terminal on recovery, never resumable execution queues. Session
deletion removes their journal with the session, not the selected directory.

Automatic compaction is restricted to completed tool batches within one live
child. It retains pinned instructions, assignment, project guidance and the most
recent batch, and replaces an older complete prefix with a bounded, reducing,
explicitly untrusted summary. Summary requests use the task's original model and
credential generation, account for usage, disable tools, and never carry opaque
reasoning into a separate provider turn. Failure stops the child without replay.
The conservative admission estimator and independent provider bounds stay intact.
A single batch that cannot fit a bounded summary request still stops truthfully.
This does not add owner-facing resume, restart continuation or provider retries.

## Safety and persistence boundaries

Compaction changes active provider context, never canonical history, credential
identity, model identity, or web provenance. It does not create a new cumulative
execution budget or extend a deadline. A checkpoint is source-bound, lossy data,
not new authority. Preserve pinned
instructions, the assignment, recent complete tool batches, verified results,
and unfinished work. Reject summaries that do not reduce context sufficiently.
Never split a tool call from its result or compact an unfinished operation.

Child execution has no cumulative provider-turn, tool-call, or mutation quota.
It remains bounded by per-operation output and timeout limits, provider context
capacity, per-entry journal and checkpoint bounds, concurrent-child limits, and
cancellation. Persist admission before dispatch and results before publishing
them. Recovery terminates interrupted or uncertain children without restarting
provider work or replaying tools. Bind the original model and credential identity;
do not silently select another provider or model.

Retain existing hosted-search admission, dispatch, success provenance, ordinal,
and privacy-policy invariants. Cancellation must cancel and drain controlled
work before terminal results.

## Current child execution bounds

Child context compaction is available only while the child is running. It records
a bounded, tool-free checkpoint before the next normal provider request and
continues with the checkpoint and retained recent entries. Checkpoint persistence
failure or cancellation stops the child; it does not continue from an unrecorded
summary.

There is no crash recovery continuation: after restart, interrupted or uncertain
child work becomes terminal and is never re-dispatched or replayed. Parent
compaction remains separate and is not changed by child compaction.

## Conservative repeated parent compaction (SQLite40)

New OpenCode root runs accepted at or after SQLite40's separate compaction
epoch, with source policy4, tool catalog/limits14, protocols1–4 and the existing
96000/32000 allowances, may use the existing four-attempt completed-batch
compaction lifecycle. Their byte-based token admission remains unchanged;
provider usage cannot authorize larger requests. Native usage policy and all
pre-epoch runs retain their previous semantics. The epoch is validated on open.
Current-user text is charged and restored when covered; current-user images
still prohibit within-run cuts. New conservative within-run cuts must remove more
text bytes than the maximum summary plus any current-user text restored by that
cut; even a maximum-sized summary must reduce active context. Historical and
native cut selection is unchanged. This historical parent change adds no
provider retry or restart continuation.

## Verification guidance

Use offline fixtures for model-specific admission, historical database replay,
checkpoint reduction and complete-batch cuts, cancellation and draining,
unchanged effect counters across compaction, terminal recovery without replay,
and exact hosted-search provenance. Exercise independent operation and context
limits separately. Live compatibility and installed-app behavior require separate
verification.
