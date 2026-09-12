# ADR 0052: Usage-based context admission and bounded within-run compaction

## Status

Accepted for implementation after owner review of the local context failure and pi0.84.2. This supersedes ADR0023's advisory-only rule and ADR0022's whole-current-run/one-compaction rule ONLY for the native root policy below. It does not authorize live QA, retries, tokenizer guesses or raised model capacities. Qualification is separate.

## Policy identity and compatibility

SQLite33 adds a singleton context-accounting epoch containing the first logical sequence of the new policy. Existing accepted runs before the epoch retain legacy execution, estimates and one whole-turn compaction. New accepted native ChatGPT roots with source policy4, Responses5, the exact six reviewed native request names, input96000/output32000 and tool catalog/limits13 use execution policy1. The immutable accepted run's sequence/profile and durable epoch bind selection, not remote returned aliases or current credentials. Unknown profiles use legacy; future policy revisions require another reviewed migration. The epoch is validated against the logical sequence counter. No historical fact is rewritten.

Canonical source hashing remains v4: exactly the same ordered canonical prefix and serialization are hashed. Execution policy1 separately governs estimates and reconstruction. Checkpoint summary estimates retain their legacy byte-based validation, including old checkpoints. SQLite33 preserves every old compaction row while replacing run uniqueness with (run, source cut) uniqueness and an indexed run lookup. Integrity validation preserves legacy restrictions and validates new cuts/counts/chronology. Missing/invalid epoch or records fail closed. Existing backup/DELETE-journal/single-worker/recovery rules remain.

## Accounting

Native text-only request admission may use the immediately preceding successfully committed provider request in the SAME run, with exact selected service/model/protocol/generation/tool policy, skills/project snapshot and checkpoint lineage. No failed, uncertain, missing or zero-input sample is usable; no other run or child can lend usage. The estimate is input + output + conservatively charged canonical additions after that request boundary +8192 request overhead. Cached input is already included once. Output is reserved in addition to visible canonical output to cover ephemeral continuation; intentional overcounting is preferable to assuming it disappeared.

Without eligible evidence, use the labelled byte-based fallback. Images continue to use conservative admission. No exact tokenizer is claimed, no universal chars/token ratio is introduced, and no subscription counting endpoint is inferred from the public API. Provider input96000/output32000 and every provider body/response validator remain unchanged. The estimate is predictive, not an exact count, guaranteed provider acceptance or spending cap. If no useful compaction can make mandatory current information fit, fail durably without hidden truncation or retry.

## Independent resources and preparation

Execution policy1 adds a hard1MiB sum of canonical source payload bytes, skills/project instructions, checkpoint text and any reintroduced current-user entry. This replaces the *implicit* small byte envelope caused by comparing bytes to tokens; it is not a model-capacity increase. Check it before materialization and again after projection. Keep256 reserved entries/items,16images/6MiB image bytes, existing per-payload/string/node/schema/continuation limits,12MiB prepared aggregate and16MiB encoded body caps.

The raw envelope bounds aggregate canonical strings; typed canonical payload validators separately bound vectors/nodes. Projection holds the bounded source and bounded projected strings, with at most one bounded individual tool serialization scratch. JSON escaping can expand source text up to6x, still subject to final provider encoding bounds. Images, continuation, queue slots and concurrent runs retain their separate existing envelopes. This is a bounded-work design, not a precise RSS promise. No tokenization or unbounded new work is introduced on the storage worker.

Both early metadata admission and late materialized admission select the same execution policy. An observation cannot bypass count/byte/image/serialization guards. Initial acceptance remains historical bookkeeping, not dispatch permission.

## Within-run compaction

At pressure, try the existing whole-turn cuts first. Policy1 may additionally cut after a COMPLETED provider tool batch inside the current run, never at an individual tool result that splits a batch. Retain at least the latest batch and the exact original user message. The original user's text and any original attachments must be charged and materialized explicitly when its canonical position is covered by the checkpoint; initially within-run cuts require that original user entry to have no images. No other original entry is resurrected.

Cut selection is bounded by the existing root tool/turn limits. Reject cuts with unmatched/crossing tool pairs, cuts not advancing the parent, cuts reaching current high water, and already attempted prefixes. Require the retained tail plus current intent/instructions/maximum16KiB summary to fit conservative admission; do not borrow a pre-compaction receipt for a changed summary. Keep the existing70% soft threshold and independent item/image pressure. After a completed compaction require a new completed root provider request before another compaction, preventing repeated summaries at the same execution boundary.

At most FOUR foreground compaction attempts per policy1 run, including any initial whole-turn/manual compaction. This conservative lifecycle bound limits churn and extra inference; it is separate from the unchanged32 completed root provider turns and any owner QA attempt ceiling. Failed/uncertain compaction stops the run, never retries or chooses another prefix. Every attempt is prepared/dispatched/terminal in the existing ledger; repeated source identity never authorizes redispatch. Summarization incurs additional provider requests even though it does not increment completed coding turns.

After checkpoint commit, clear ephemeral provider continuation and recreate the native turn before constructing the next request. Rebuild from fixed instructions, summary, explicitly retained current user intent and contiguous recent canonical tail. Full original history remains intact. Source projection keeps the existing marked48KiB excerpts, bounded parent/guidance/summary, full-prefix digest and !! exclusion. No tools or external effects are executed while summarizing. Cancellation drains the controlled request; recovery terminates interrupted operations without replay.

## Other request classes and presentation

OpenCode roots, children, images and background maintenance retain their existing conservative policies in this increment. No parent receipt is reused for a child or a summarizer. Background maintenance still works only on completed whole-turn prefixes and installs at a later run's first request. This deliberate limited rollout is not a claim of pi equivalence for all providers.

IPC45 must label the selected accounting policy, actual admission estimate and source-byte envelope separately from the legacy byte-heavy estimate. Status remains metadata-only, not request preparation, image I/O or a billing assertion. Retained old facts/failed runs keep their meaning. No remote-error compact-and-retry is added: pi's regex/length heuristics do not establish certainty under Morons' no-replay contract.

## Qualification

Use owned fresh stores and loopback providers only. Preserve the earlier failed-before fixture separately. Tests must cover same-run usage/cache accounting, wrong/missing/uncertain/changed checkpoint receipts, low-estimate resource rejection, complete batch cuts and exact user intent, at least two advancing compactions, attempt cap/no immediate recompaction, continuation reset, !! exclusion, cancellation/uncertainty/recovery, old schema row preservation and corrupt provenance. Preserve existing legacy tests; new behavior gets new fixtures. Run frozen-source format/check, focused and full locked-offline tests, warnings-denied debug/release Clippy and build/dependency checks. No retained profile migration or live reliability claim follows from these tests.
