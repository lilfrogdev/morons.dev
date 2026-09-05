# ADR 0022: Release context and provider hardening

## Status

Accepted for implementation

## Reference

OpenAI Codex is Apache-2.0 open source. This review pins [commit 459a79eb85400af759e9220c7bafb4429ae07516](https://github.com/openai/codex/tree/459a79eb85400af759e9220c7bafb4429ae07516), particularly `codex-rs/core/src/session/context_window.rs`, `session/turn.rs`, `compact.rs`, and `compact_token_budget.rs`.

Gemini safety-field review additionally uses Google's fixed public [Generative Language v1beta discovery document](https://generativelanguage.googleapis.com/$discovery/rest?version=v1beta): `SafetyRating` has required `category` and `probability`, and optional `blocked`. Only its Gemini categories are admitted; PaLM categories, Vertex-only score fields, and unknown nested fields are rejected.

Codex distinguishes soft compaction thresholds from hard context limits, reserves headroom, and builds bounded recent-history replacement contexts. Its local summarization path can trim oldest input on context overflow and retry requests. Its token-budget path can start a new context window without model summarization. These are references, not code imported into Morons.

## Decision and boundaries (documented before implementation)

- Preserve canonical transcripts and existing policy-v4 source-bound lossy checkpoints. Never silently discard canonical entries or execute a transcript tool while summarizing it. A checkpoint is untrusted context, not authority or current filesystem state.
- Check entry counts and image count/byte pressure as well as the seventy-percent estimated-token threshold. Reserve space for developer instructions, skills, checkpoint text, and subsequent tool turns. Keep recent complete user turns when they fit; shorten that retained tail before crossing a hard limit. Never split the current run or a tool call/result pair.
- Admission may accept a compaction-capable run when old history fills ordinary context. The acceptance estimate saturates at the model limit to signal full context; it is not a dispatch estimate. Recompute and validate the real request budget before every inference. Current user input and attachments remain individually bounded, and sessions still have one run and no input queue.
- A manual compaction request remains available when old history is full. Compaction is a bounded, one-attempt operation, not a retry of uncertain work. Preserve the current run, exact service/model, and context-excluded (`!!`) filtering. Summarization sources may be explicitly projected to a bounded recent excerpt of the covered prefix when the prefix cannot fit; disclose omitted/truncated input to the summarizer. The checkpoint digest still covers the complete canonical prefix, including excluded entries, whose text must never enter provider context.
- Load the active suffix rather than cloning the complete canonical transcript for every tool turn. Validate checkpoint digests by streaming bounded canonical pages at startup and after external SQLite changes. The exclusive storage worker owns canonical writes; new checkpoints are bound to a freshly computed complete source digest before commit. No hidden cross-session memory or persistent provider continuation is added.
- A recoverable execution/context failure must become durably terminal after controlled work stops. Unexpected persistence/integrity failures request server shutdown rather than pretending terminalization succeeded. Recovery continues to mark uncertain external effects without replaying them.
- Gemini safety ratings use a closed reviewed schema with bounded categories/probabilities and a typed blocked flag; blocked or contradictory candidate state must never yield executable tools. Chat cache accounting uses checked arithmetic.
- An immutable prepared provider request retains its single bounded serialized body. Encoding is not repeated at dispatch; credentials are still leased only after request validation and never enter the retained body.

The application protocol remains v36, persistence schema v25, and canonical context policy/digest format v4. One compaction operation per run remains the durable limit; a completed compaction is never repeated within that run. New summaries are capped at 16 KiB, projected source excerpts at 48 KiB plus a bounded omission notice, and guidance at 8 KiB. Older larger checkpoints are still validated and can be compacted through an explicitly bounded parent-summary excerpt.

The changes add no provider origin, automatic external retry, remote model admission, credential scope, or dependency. Data-only catalog refresh remains an availability intersection with the reviewed built-in manifest. No Codex credentials, OAuth integration, remote compaction endpoint, model fallback, hooks, or queue is imported.

## Validation

Regression coverage must include short-message entry pressure, manual recovery at capacity, oversized tool results, image pressure, complete-turn retention, hidden-command exclusion, bounded source projection, checkpoint corruption after reopen/external change, cancellation/failure during compaction, blocked/unknown Gemini safety metadata, malformed Chat cache counts, and immutable single-encoding dispatch. Run formatting, all-target/all-feature Clippy with warnings denied, dependency checks, and the workspace tests before claiming completion.
