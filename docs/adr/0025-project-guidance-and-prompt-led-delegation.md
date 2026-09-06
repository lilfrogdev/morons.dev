# ADR 0025: Project guidance and prompt-led delegation

## Status

Accepted

## Decision and boundary (before implementation)

Use a small shared coding core, separate parent/child role guidance, tool-specific guidance, and separately labeled project context. Prefer simple, understood changes: reuse existing code, standard-library/native features and suitable installed dependencies; avoid speculative abstractions; preserve required validation, error handling, accessibility and tests. New explanatory comments should add context rather than repeat code and stay on one line unless the user asks otherwise; required notices/documentation remain intact. Explicit user preferences override style/workflow defaults, not harness constraints.

The main run's selected model plans implementation work, delegates through `task`, reviews results and reports verification. Discussion-only requests need not spawn children. This is a prompt preference, not a tool restriction, permission gate or guaranteed orchestration mode. Parent tools remain available; children cannot delegate recursively. Existing server-authoritative subagent model selection, immutable batch pinning, disclosures, cancellation, quotas and no-fallback/no-replay rules remain unchanged. No prompt or project file chooses a model or billing identity.

## Project context

On new input, discover guidance before durable acceptance, outside the SQLite worker. Read Morons-global guidance from `~/.morons`, then ancestors from filesystem root through the selected directory. In each directory choose the first present candidate: `AGENTS.override.md`, `AGENTS.md`, `AGENTS.MD`, `CLAUDE.md`, `CLAUDE.MD`. A present but invalid preferred file produces a warning, not fallback to a lower-priority sibling. An override shadows only that directory's other candidates. Deduplicate paths; do not recursively scan descendants, execute files, follow references automatically, or read Pi/Codex configuration. Files for deeper scopes can still be read explicitly with normal tools. No Git subprocess or worktree-specific context inference is added.

Automatic discovery transmits the selected instruction text to the chosen inference service. Treat it as untrusted project guidance below explicit user instructions and harness policy. It cannot replace the core, grant authority, change fixed routing, or expose Morons-managed credentials intentionally. It is not a sandbox or protection from arbitrary same-user processes. Do not place secrets in automatically loaded instruction files. Reject final-component symlinks and special files rather than automatically reading their targets. Normal ancestor/path semantics remain; this is not filesystem confinement.

Bound discovery to 64 ancestors, 16 files, 16 KiB per file, 32 KiB total content, 16 warnings and 64 KiB serialized context, with a cooperative five-second deadline and bounded blocking-job admission. Check the deadline between directory and read operations; ordinary OS filesystem operations can still block in the kernel. Never silently truncate instruction text. Surface skips/limits as bounded warnings. Setting `MORONS_NO_PROJECT_CONTEXT` in the server environment disables discovery (any value); unset it to enable. It is captured at server startup, not controlled by repositories or model prompts.

Pin the bounded files, warnings and enabled state atomically with each new tool-enabled run, in SQLite, with a run-bound integrity digest. Exact input retries return the existing run without rediscovery. Later turns and child agents use that immutable snapshot, even after file edits; the next newly accepted run refreshes it. These are instruction snapshots, not repository copies or filesystem snapshots. Archive/delete never changes source files. Context compaction neither summarizes nor authorizes project guidance; it remains a separate bounded prefix. Charge its actual rendered bytes to admission and inference budgets; provider-usage observations require identical pinned project context.

`/context` reports the last accepted run's loaded paths and warnings without rereading source files or returning their contents over IPC. A new/legacy session has no snapshot to report. No automatic discovery occurs during command execution, checkpoint summarization, historical replay or recovery. A newly submitted `/compact` input pins guidance like any other accepted input, but the compaction request does not include that guidance.

## Compatibility and validation

Application protocol advances to 38 for context metadata; SQLite schema to 26 for run instruction records; tool catalog/limits policy to 9 for the changed instruction contract (numeric tool limits unchanged). Canonical transcript/compaction policy and digest remain v4. Versions through 8 retain migration/integrity support and never acquire newly discovered instructions retroactively. Interrupted runs still terminate on restart and are never replayed.

Test discovery order, precedence, bounds, invalid nodes/UTF-8, opt-out, isolated sessions, pinning/refresh/retries, source warnings, child propagation, context accounting/observation invalidation, deletion non-interference, corruption and legacy migration. Test shared core/role/tool composition and terminal-safe metadata rendering. Run formatting, locked workspace checks/tests/build, Clippy with denied warnings and dependency policy checks.

## References

- Pi 0.84.2 installed `dist/core/system-prompt.js`, `resource-loader.js`, tool prompt contributions and README context-file documentation. Morons adopts the composition/order, not unbounded reads, project SYSTEM replacement or Pi's storage/lifecycle.
- Ponytail, MIT, reviewed at `974d940a1c5344210874150b98ff0d2c861fab6a`: https://github.com/DietrichGebert/ponytail/blob/974d940a1c5344210874150b98ff0d2c861fab6a/skills/ponytail/SKILL.md . Independently worded principles, not a bundled skill, dependency, performance claim, code-golf rule or reduction in test/security obligations.
