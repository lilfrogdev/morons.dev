# ADR 0013: Bounded batched subagents

## Status

Accepted.

This decision amends ADR 0012 by including deliberately bounded subagents in the MVP. ADR 0012's trusted-local authority, direct-working-directory, provider-custody, durability, cancellation, and no-managed-worktree decisions continue to apply.

## Context

Focused child agents can keep the user-facing model context small and perform independent investigation concurrently. Morons already has a typed provider tool loop, a fixed trusted tool catalog, durable outer tool operations, direct local tools, and server-owned OpenCode credentials. A subagent design should reuse those boundaries rather than introduce a second agent runtime or make the persistent IPython kernel authoritative for orchestration.

[OMP exposes delegation as a `task` tool](https://github.com/can1357/oh-my-pi/blob/main/docs/tools/task.md) and supports a batch with shared context plus one independent assignment per child. [Prime Agent exposes recursive child admission through its Python RLM bridge](https://github.com/PrimeIntellect-ai/prime-agent/blob/main/packages/coding-agent/docs/rlm.md), retains child registries, and routes results through asynchronous messaging. The RLM model is useful for a Python-centered harness, but Morons exposes several small provider tools directly and intentionally treats IPython memory as temporary. Making child lifecycle depend on that kernel would add another protocol, registry, persistence model, and asynchronous context-injection path.

Morons therefore adopts the useful part of OMP's design: a typed batched `task` tool whose children receive scoped context and return bounded reports. It does not adopt OMP's agent discovery, background jobs, revival, peer messaging, worktree isolation, or artifact URL layers for the MVP.

## Decision

### Model-facing contract

The fixed tool catalog adds `task`. One call has this closed shape:

- `context`: required bounded shared background supplied once to all children; and
- `tasks`: one to three items, each containing an optional bounded ASCII name and one required bounded self-contained assignment.

The parent should use a batch for independent work and assign disjoint mutations. The server rejects empty assignments, duplicate names, unknown fields, excessive bytes, excessive children, and more than two `task` calls in one top-level run.

The tool blocks the parent tool loop until every admitted child reaches a terminal result. Children run concurrently under one global four-child semaphore. Results are returned in input order regardless of completion order. Each result contains its index, optional name, terminal status, pinned model disclosure, bounded final report, provider-turn count, tool-call and mutation counts, and bounded provider usage. The complete `task` call and result are canonical parent transcript entries and enter later parent context.

There is no background child registry, result injection, child browser, messaging bus, idle revival, or independently resumable child session in the MVP. This avoids hidden work after the parent continues and makes result ordering deterministic.

### Context and token efficiency

A child receives only:

1. a fixed server-owned child instruction and tool contract;
2. the selected working-directory locator;
3. the call's shared `context`; and
4. that child's assignment.

It does not inherit the parent transcript, compaction checkpoint, images, reasoning continuation, active skill body, IPython memory, or sibling context. The parent must place necessary constraints and findings in `context` or the assignment. This explicit narrow handoff avoids retransmitting a potentially large parent conversation for every child.

ADR 0018 amends the model-routing part of this decision. Children inherit the parent's exact reviewed service, model, and model limits by default. A typed global setting may instead select one exact reviewed child service/model pair; the executor resolves and pins that pair once per `task` call and never silently substitutes another. Children continue to use the parent's accepted credential generation. Each child has at most eight provider turns, twenty-four tool calls, eight mutating calls, 32 KiB of final report text, an 8,192-token output request, and ten minutes within the outer tool operation. Child context is conservatively estimated before every dispatch and is not compacted; a child that no longer fits returns a resource-limit result.

### Child capabilities and authority

Children share the parent's selected working directory and normal local-user authority. They receive `read`, `write`, `edit`, `bash`, and `web_search`. They do not receive `task`, so recursion depth is exactly one. They do not receive `ipython`, because concurrent children sharing the session's temporary persistent kernel would create implicit cross-child memory and execution races. Child `read` rejects image results rather than creating hidden attachment persistence.

Concurrent children and the parent can observe stale state or race in the shared directory. Prompt guidance asks the parent to assign disjoint mutations and children to re-read before editing, but this is coordination guidance rather than isolation or authorization. Morons does not create worktrees, copies, branches, snapshots, or sandboxes for children. Users requiring isolation still wrap the complete application externally.

### Provider affinity and credentials

Every child is a separate OpenCode conversation. Trusted code derives one opaque child conversation identity from the durable parent session identity, canonical parent tool-call identity, and one-based child index using a domain-separated SHA-256 construction. The child value is stable across that child's provider turns, differs from the parent and siblings, and does not expose a raw Morons locator. The existing provider adapter derives and sends the `x-opencode-session` header and retains its inference-only redaction and routing rules.

Every child turn checks the parent's accepted credential generation immediately before dispatch. Credential bytes remain in the existing server provider boundary and are never placed in child prompts, tool arguments, environments, results, persistence, logs, or protocol messages. No child inference is retried automatically after dispatch.

### Durability, recovery, and cancellation

`task` is one durable outer tool operation, analogous to `bash`: the server commits the typed parent call, prepares and marks the operation dispatched, then runs nested provider and tool activity. Nested child turns are bounded implementation activity rather than independent canonical session entries. A completed result commits all child reports and usage before the parent can continue.

A crash or task-executor failure after outer dispatch never restarts a child or repeats an inference, command, web request, or filesystem effect. Startup terminates the incomplete mutating `task` operation as uncertain using the existing tool recovery path. This accurately covers unknown nested provider usage and local effects without inventing child completion.

Parent cancellation fans out to all running and semaphore-waiting children. The executor drains every child after cancellation or timeout before returning. Child shell and web operations use their existing cancellation contracts and process-tree controls. Cancellation and timeout do not roll back effects that already completed, and upstream provider billing may remain uncertain.

Child provider failures, malformed output, context exhaustion, and ordinary tool failures become bounded per-child results when the outer operation itself completed and can report them. A panic or lost executor outcome makes the outer operation uncertain rather than fabricating child results.

### Versions and validation

The fixed tool catalog and limits version becomes 8, the application protocol becomes 30, and the persistence schema becomes 23 so canonical tool-kind constraints admit `task` as kind 14. Historical tool catalogs remain decodable and valid only for the versions that offered them.

Validation covers strict batch decoding, bounds, duplicate names, child tool filtering, global parallel dispatch, scoped context, deterministic result ordering, child tool loops, provider usage, cancellation, stable distinct OpenCode session headers, canonical parent transcript output, migration, corruption rejection, and restart recovery without replay.

## Consequences

- The parent can parallelize up to three focused assignments with one compact shared-context handoff.
- Child context and results remain explicit, bounded, and visible to the parent rather than becoming hidden memory.
- Morons gains useful concurrency without a second Python host API, background scheduler, child-session browser, worktree manager, or messaging protocol.
- Children can modify the real directory concurrently and therefore introduce race risk under the accepted trusted-local model.
- Child internal transcripts are not independently resumable; only the canonical `task` call and bounded terminal result are durable.
- Subagent requests consume provider quota and may increase cost even when the parent ultimately fails or is cancelled.

## Alternatives rejected

- Prime Agent's Python-native RLM child registry would make a temporary kernel part of the orchestration surface and require durable handles, messaging, observation, deletion, restoration, and asynchronous result injection.
- Copying OMP's background jobs, idle revival, peer messaging, agent discovery, and artifact protocols would substantially enlarge the MVP and retain hidden work after the parent proceeds.
- Giving every child the complete parent transcript would multiply context tokens and reduce the main benefit of delegation.
- Recursive `task` access would multiply cost and concurrency and make aggregate limits harder to reason about.
- Sharing persistent IPython across children would create implicit cross-child state and race execution counts.
- Managed worktrees or patches would reintroduce the repository-isolation architecture rejected by ADR 0012.
- Sequential child execution would retain context separation but lose the latency benefit of batch delegation.
