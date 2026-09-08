# ADR 0040: Rebuildable run execution counters

## Status

Accepted before implementation, 2026-09-08. Focused persistence correction independent of native authentication and hosted search.

## Evidence and decision

The live provider counter increments after a completed outcome, but restart reconstruction counts Prepared operations. Direct mutation-capable tools were added without extending reconstruction's legacy-only tool-kind predicate. These rebuildable counters can therefore disagree with their canonical facts. A completed final response racing cancellation also commits its receipt before returning without incrementing the live counter.

Define `provider_turns` as the number of committed Completed provider-operation facts, including a validated response whose text is discarded because cancellation won. Prepared, Dispatched, failed, uncertain and interrupted operations do not count as completed turns. Update the live counter in the same transaction as the receipt, before the cancellation branch, and reconstruct from Completed facts. This counter is not total attempts, billing, or permission to retry.

Define `tool_mutations` as the number of committed mutation-capable tool calls, consistent with `ToolKind::is_mutation`, not a claim that an effect ran or changed a file. Include legacy mutation kinds and direct write/edit/Bash/IPython/task calls; read/search calls remain excluded. Pending tools still stop/recover without replay. Tool-result byte accounting remains byte-based on existing BLOB payloads.

No canonical input, receipt, usage, run outcome, fact sequence, provider identity, credential, request bytes, schema or protocol is rewritten. Existing startup reconstruction may correct these derived counters; interrupted runs remain terminal after recovery and no external work restarts. Preserve acceptance receipts' original zero counters and all historical failure evidence. No budget increase, authorization change or rollback guarantee is introduced.

## Validation

Use real storage-worker transactions and restart recovery: prepared-only/dispatched-uncertain/completed operations, completion racing cancellation, current mutation/nonmutation tool kinds, and preserved canonical receipt counts. Regressions must fail before the correction. Run locked local gates and exact-head platform/security checks. Do not migrate retained QA or read credentials as part of this test.
