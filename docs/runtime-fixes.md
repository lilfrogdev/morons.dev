# Runtime fixes

Steering remains production-disabled while these fixes are implemented and verified.

## Context-overflow recovery

- Distinguish a structured HTTP 400 `context_length_exceeded` rejection on the native Codex route from incomplete streams and generic resource limits.
- Read the complete bounded error body before classifying the rejection. Do not retain or expose its message, request content, or credentials.
- Commit the rejected attempt before bounded same-run compaction, rebuild context, and dispatch with the accepted model, credential generation, and policy bindings unchanged.
- Permit at most one overflow recovery per run. Failed compaction or repeated overflow stops the run.
- Use the committed failed provider operation with `ResourceLimit` as a conservative consumed-allowance marker; any earlier failed or uncertain operation bars recovery. Commit settlement before planning, require a safe compaction before redispatch, and let ordinary restart recovery terminate interrupted runs rather than resume recovery.
- Never infer retry permission from a partial response, stream finish reason, timeout, or generic resource-limit error. Preserve successful responses and completed tools; do not replay them.
- A proven context rejection during compaction settles that dispatched compaction as failed, never uncertain; its attempted prefix remains blocked from replay. Other dispatched compaction failures remain uncertain.
- Show exactly `Compacting` during foreground compaction.

This narrowly extends the no-retry boundary only for proven request rejection, not uncertain execution. Error classification alone must not initiate a retry.

## Premature completion

- Keep explicit execution instructions: a final answer ends the run and schedules nothing.
- Add a bounded checkpoint before committing `Succeeded`; do not reopen terminal runs.
- Evaluate the authorized task and available evidence, not keywords in the candidate final answer. Distinguish completed work, discussion, necessary questions, concrete blockers, and actionable unfinished work.
- Allow at most one same-run continuation, retaining committed work and tool results. The checkpoint cannot grant new authority, select another model or credential, override cancellation or uncertainty, or replay external effects.
- Treat model review as fallible. Verify planning-only endings, valid questions, blockers, completion, cancellation, failure, and continuation exhaustion before enabling the checkpoint.
- Persist the checkpoint decision and continuation consumption before dispatch; restart recovery must not repeat an uncertain review or continuation.

The completion checkpoint is planned, not enabled. It is separate from proven-overflow recovery and from steering delivery.

## Other outstanding verification

- Native context-capacity changes require migration, admission, integrity, and reopen coverage; a manifest-only increase is insufficient.
- Verify silent Bash execution beyond the former deadline without changing network or Python limits.
- Review the fixes independently of the paused steering changes; run targeted regressions, formatting, workspace Clippy, and the relevant full suites before release.
