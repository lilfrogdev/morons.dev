# ADR 0023: Context observation and request preparation

## Status

Accepted for implementation

## References

These are design/source references, not dependencies:

- [Pi 0.84.2, `core/compaction/compaction`](https://github.com/earendil-works/pi-mono/blob/v0.84.2/packages/coding-agent/src/core/compaction/compaction.ts): reuse the last successful provider usage and estimate the subsequent tail rather than re-estimating the entire prompt blindly.
- [OpenCode 5cf9f517](https://github.com/anomalyco/opencode/blob/5cf9f517cfec3ef68d3e68a12a6a4b3163947f44/packages/opencode/src/session/overflow.ts): distinguish usable input capacity, output reserves and provider-reported context consumption.
- [OpenAI prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching): stable tool definitions and instruction prefixes support reuse; routing affinity is not a cache-hit guarantee.

## Decision and boundary (before implementation)

- Use successful, committed, same-service/model/protocol/tool-policy provider usage only as an advisory estimate for proactive compaction and context display. The indexed lookup examines at most the eight newest runs; older/unavailable observations fall back rather than scanning retained history. The observation must follow the current checkpoint and match the active skill snapshot; a changed model, policy, checkpoint or skill set falls back to conservative accounting. Add a conservative estimate of every new canonical item after the observed request boundary, including images, and reserve output/continuation and request overhead. Do not count cached input twice.
- Never lower the existing conservative dispatch/admission guards or hard item/byte/image limits based on remote usage. Hard pressure still triggers compaction even if the advisory token estimate is low. This avoids adding unreviewed tokenizers or pretending a universal bytes-per-token ratio is safe.
- Context status is an observation, not execution preparation. Read bounded SQLite metadata and budgets without reading attachment bytes, projecting summary source, or constructing provider requests. Checkpoint integrity is still enforced; attachment bytes are validated at startup and actual dispatch. A status response is not an integrity attestation for attachment bytes.
- Expose estimate provenance, conservative guard consumption, and the latest completed root provider call's cache/input/output/timing counters. Label their scope; never imply these counters include subagents, compaction, failed calls, or a complete monetary bill. Compaction count/timing may be shown from its existing operation records; missing provider billing data is not zero cost.
- Prepare immutable built-in root/child tool definitions once, including bounded validation and lazy Gemini schema projection. Retain per-request model/capability checks, bounded dynamic-input validation and final encoding bounds. Dynamic tool sets still validate independently. No arbitrary key-based cache, remote-selected schema or unbounded memoization is added.
- Preserve default prompts, exact fixed routing, credential custody, one top-level run per session, canonical history, source digest policy v4 and no automatic replay. Background compaction and OAuth remain separate future work.

Application protocol v37 adds the observation fields; SQLite schema remains v25, canonical context policy/digest format v4 and tool catalog/limits v8. The `/context` dialog distinguishes advisory consumption from the conservative guard and labels incomplete usage scope. Unit-test reorganization preserves test bodies and platform gates; old migration tests stay, with names corrected to the current schema target.

## Validation

Compare pre/post test inventories; preserve all assertions and ignored/native gates. Add same-prefix and invalidated-observation tests, independent hard-budget tests, corrupt-checkpoint and unavailable-image status/dispatch tests, and immutable/prepared versus fresh request equivalence for all wire families. Record reproducible local timing probes without flaky timing assertions or live inference. Run formatting, locked workspace checks, all-target/all-feature Clippy with warnings denied, dependency policy, full tests and platform CI.
