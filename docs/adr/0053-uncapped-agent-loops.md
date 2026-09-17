# ADR 0053: Uncapped agent loops

Status: Accepted

## Decision

Parent runs and child assignments have no cumulative provider-turn, tool-call,
mutation, Task-call, tool-result-byte quota, or whole-run deadline. They continue
until completion, cancellation, an operation failure, or a retained resource bound.
This removes the former 32/64/16 parent and 8/24/8 child quotas, two-Task quota,
2 MiB cumulative result quota, and 30/10 minute run deadlines.

## Resource and security boundary

An admitted run can now consume time, disk and paid provider usage until stopped;
there is no automatic overall spend or elapsed-time stop. This is not a sandbox.
Per-operation timeouts, bounded requests/results and provider context, per-turn
batch limits, concurrency, attachment/storage limits, authentication, validation,
and cancellation/draining remain enforced. Context exhaustion can still stop a
run; uncapping does not introduce automatic continuation or retries.

Counters remain checked, nonnegative machine integers for accounting, never
wrapping identities. SQLite migration widens accounting constraints without
rewriting historical facts. Historical tool results remain readable. No failed
or uncertain provider/tool operation is replayed, and cancellation is terminal
only after supervised work stops. No deployment or provider service/model identity
changes are part of this decision. Ephemeral owned-web invocation IDs use a v2
hash domain with a full-width u64 child-tool ordinal to avoid truncation collisions;
owner-operation and child scope remain bound. Historical results are not rehashed,
and interrupted invocations are never resumed or replayed.
