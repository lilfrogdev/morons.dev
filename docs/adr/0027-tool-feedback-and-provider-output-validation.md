# ADR 0027: Tool feedback and provider-output validation

- Status: Accepted
- Date: 2026-09-05

## Context

Useful tool failures must be distinguishable from uncertain effects, and strict provider decoding must match the reviewed wire contract rather than reject legitimate responses. Prompt-led delegation also needs explicit assignment ownership and visible tool budgets. Run-specific QA evidence remains outside the repository; regression fixtures establish the maintained behavior.

## Decision

- Before file mutation, perform read-only parent/target metadata checks. Missing parents and known wrong node types fail without creating directories, opening/truncating a target or claiming uncertainty about a write that was not attempted. Recheck cancellation afterward. Errors once mutation is attempted remain uncertain, including races after preflight. Ordinary symlink/absolute/parent-path semantics remain available; these checks are not confinement or rollback.
- Keep read limits unchanged. State the one-based offset and maximum/default line count in descriptions that survive Gemini schema lowering; dropping unsupported numeric schema keywords must not hide the operational contract from the model. Also state that Bash accepts only `command`, not model-selected timeout, environment or working-directory fields.
- Enable Responses strict function schemas for built-in tools. Every object property is required; an optional task name is represented as nullable. Dynamic tool definitions explicitly select their own strict flag. Other wire families retain their existing schema conversion. This asks the provider to obey the schema, but local validation remains authoritative; unsupported/ignored strict behavior never authorizes extra fields, larger reads, fallback or retry.
- Use fixed server-owned rejection labels and bounded enum/boolean classifications on server stderr. Never include untrusted field names, text, arguments, paths, identifiers, provider bodies, credentials or opaque continuations. Invalid tool arguments, unknown fields, contradictory message phases and malformed provider data remain rejected. Diagnostic logging does not retry inference or tools.
- Accept bounded `thoughtSignature` on Gemini text parts, including empty final streaming text. Google documents this for non-function-call output and states that omitting it on the next request does not cause an error. Discard these optional signatures without persisting, logging or exposing them; charge them to the aggregate ignored-reasoning budget. This intentionally forgoes their possible reasoning-continuity benefit. Preserve existing exact ephemeral replay of function-call signatures and closed part/usage/safety validation.
- Clarify prompt defaults: parent inspection/planning first; one implementation owner for small changes; parallelize only independent assignments, not dependent implementation and verification; review changed code and verify after mutation. After a failed child, stop and report partial effects rather than silently retrying, delegating the same work again or taking over without an explicit user override. Child instructions disclose existing turn/tool/mutation limits and require a final report within them. These remain prompt preferences, not new orchestration or tool restrictions.
- Label the current run's project guidance as superseding historical guidance reports; disabled discovery does not authorize treating old transcript markers as a current snapshot. Do not rediscover an active run's files or remove canonical history.

## Compatibility and verification

Tool catalog/limits policy advances from 9 to 10 to bind the new prompt/description and preflight behavior; numeric tool limits do not change. SQLite remains 26 and application protocol remains 38: no record layout or IPC DTO changes. Validate both v9 and v10 project snapshots explicitly, retaining older history/no-retroactive-guidance rules. Historical acceptance estimates use the exact v9 rendered-guidance prefix length; changing current prompt wording must not invalidate previously accepted facts. Provider wire-family revisions remain unchanged; this is a reviewed correction within Gemini revision 4, not a new route or protocol fallback.

Regressions cover known preflight failures, post-preflight races/cancellation, text signatures and aggregate bounds, unchanged forbidden/malformed output rejection, schema-visible limits, prompt role isolation, legacy snapshot compatibility and current snapshot refresh. Live model adherence is separate evidence and is not guaranteed by prompt tests.

Reference: [Google's thought-signature contract](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/thought-signatures), especially non-function-calling and empty streaming text parts; also [Gemini API documentation](https://ai.google.dev/gemini-api/docs/thought-signatures). No dependency or external credential state is imported. Background compaction remains outside this change.
