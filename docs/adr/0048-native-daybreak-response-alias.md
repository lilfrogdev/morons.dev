# ADR 0048: Reviewed native Daybreak response-model alias

## Status

Accepted before implementation. This narrowly supersedes ADR0035's exact response-name requirement for one documented Daybreak resolution; all other model admission and response validation remains unchanged.

## Evidence and problem

The owner reports working Daybreak access in Pi. Two retained Morons samples failed at the closed `response-model` stage. That stage rejects a response model string unequal to the requested ID in either lifecycle or completed-response validation. Neither failed payload nor actual returned name was captured; their usage remains unknown and their facts must not be rewritten or replayed.

OpenAI's public [Daybreak Blue model page](https://developers.openai.com/api/docs/models/gpt-daybreak-blue-latest), reviewed 2026-09-10, describes a moving flagship alias with defensive-cybersecurity safeguards and explicitly lists `gpt-5.6-sol` as its current underlying snapshot. The approval requirement remains. This public API documentation supports a concrete compatibility case; it is not proof of the contents of either failed native response, subscription entitlement, or future alias targets.

Reviewed installed Pi AI0.84.2 public source sends `model.id` in its Codex request and initializes output attribution with that selected ID. Its Codex event mapping/shared Responses stream processor does not require response-model string equality. The default provider resolves to the same native Codex Responses route. No private Pi configuration/authentication file, credential, raw provider response or hidden reasoning was read or imported. Pi's broad acceptance, automatic retries/transport fallback and remote metadata authority are not adopted.

An independent loopback transport regression on the unchanged main source sends `gpt-daybreak-blue-latest`, returns otherwise valid `gpt-5.6-sol` lifecycle/completion envelopes, and fails the intended success assertion. It actually ran and failed (not a compiler error or a zero-test pass). This establishes a reproducible documented-alias incompatibility, not the historical live payload's exact cause.

## Decision and boundary

Keep the requested model, UI selection, root/task/compaction bindings and canonical attribution exactly `gpt-daybreak-blue-latest`. Never replace its request with a plain Sol request: that would change the requested safeguards/approval identity. All existing service, credential-generation, fixed-route, capability, data-use, context and usage admission stays tied to the explicitly selected Daybreak profile.

Only the native Codex decoder may additionally accept the exact response-model string `gpt-5.6-sol` when the expected model is exactly `gpt-daybreak-blue-latest`. Define this directional pair in reviewed static native code, not a caller option or remote catalog. Both the selected alias and its single reviewed resolution are equivalent for this comparison at lifecycle and completed-response checks. Existing optional lifecycle-model behavior is retained; a completed model still must have its existing required string shape.

Every other native expected model and every non-native Responses decoder retains exact equality. No case folding, trimming, date/prefix matching, arbitrary model acceptance, alias chaining, reverse mapping, remote refresh or generic fallback. If OpenAI changes the moving alias to another name, fail closed until that target is independently reviewed. Unsupported echoes still produce the existing redacted `response-model` diagnostic and poison the dispatched turn without retry.

The accepted response name never selects an origin, credential, tool capability, profile, billing identity, output limit or future request. No response value is newly persisted, logged or exposed. HTTP/SSE framing, response IDs, lifecycle/sequence, complete output reconciliation, usage validation, receipt-bound continuation, cancellation, poisoning and no-replay checks are unchanged. OpenAI hosted search keeps its separate exact GPT-5.5 contract; OpenCode routing/decoding is unchanged.

This compatibility change adds no dependency, unsafe code, knob, schema/IPC revision, request parameter, access-program assertion or runtime-limit change. Native full Responses remains protocol5; the requested model/route and wire grammar are unchanged. Existing failed runs remain failed, independently of a later compatible implementation or fresh successful sample.

## Validation

- Failed-before loopback test, then success with unchanged Daybreak request identity, fixed route/headers, receipt/usage and no prepared-request replay.
- Exact alias in created/in-progress/completed envelopes; mixed selected/resolved names remain within the one reviewed equivalence pair. Unrelated names at each checked stage reject.
- Reject reverse mapping, other native models, non-native decoders, unknown/new snapshot names, prefixes, case/whitespace/control variants. Do not silently drop old rejection coverage: replace the now-reviewed Sol case with an actually unreviewed target and retain authorization/no-retry assertions.
- Exercise Daybreak's resolved echo in the existing six-model root image/tool/continuation flow while every dispatched request and durable accepted model remains the selected Daybreak ID.
- Focused regressions, formatting, warnings-denied Clippy and full locked workspace gates on frozen source. Keep synthetic traffic, live-provider samples, platform CI and signed release qualification distinct.
- Any live confirmation uses a newly scoped one-shot request on the qualified candidate, not replay of an uncertain run. Missing receipts remain unknown; a failure does not authorize another alias, relaxed guard or retry.
