# ADR 0021: Complete OpenCode Zen model and protocol support

## Status

Accepted

## Context

OpenCode Zen's public `https://opencode.ai/zen/v1/models` catalog exposed 66 unique model identifiers during the 2026-09-04/05 review. Morons admitted only 19 Responses-routed Zen entries. The live service additionally exposes Anthropic Messages, OpenAI-compatible Chat Completions, and Google Gemini models, plus newer and older Responses models.

The catalog response contains identifiers only. It cannot safely decide a route, wire protocol, authorization header, capability, limit, or data-use disclosure. Model names also do not identify a protocol reliably: MiniMax uses Anthropic Messages on Go but Chat Completions on Zen, while Qwen uses Anthropic Messages on both services only for specifically documented model generations.

OpenCode's own implementation provides the interoperability reference requested for this review. Source commit `5cf9f517cfec3ef68d3e68a12a6a4b3163947f44` contains:

- the Zen model/endpoint/SDK table in `packages/web/src/content/docs/zen.mdx`;
- SDK selection and OpenCode provider setup in `packages/opencode/src/provider/provider.ts`;
- provider-neutral dispatch in `packages/opencode/src/session/llm/native-request.ts`;
- exact request lowering, routes, authentication conventions, SSE parsing, tool behavior, reasoning metadata, and usage mapping in `packages/llm/src/providers/` and `packages/llm/src/protocols/`; and
- protocol tests and recorded provider streams under `packages/llm/test/provider/` and `packages/llm/test/fixtures/recordings/`; and
- the frozen OpenCode Zen model metadata in `packages/opencode/test/tool/fixtures/models-api.json`.

The current documentation has route rows for 63 of the 66 live identifiers. `claude-sonnet-4`, `deepseek-v4-flash-free`, and `laguna-s-2.1-free` remain live without a current endpoint-table row. The pinned OpenCode model fixture still maps Claude Sonnet 4 to Anthropic Messages and DeepSeek V4 Flash Free to the provider's default OpenAI-compatible Chat route. OpenCode's current provider code routes an entry without a per-model SDK override through the Zen provider's OpenAI-compatible base, and a credentialed 2026-09-04 request confirmed Laguna S 2.1 Free on the fixed Chat Completions route without fallback. Morons freezes those reviewed results rather than consulting remote routing metadata at dispatch time.

## Decision

Morons carries a static reviewed snapshot for all 66 Zen identifiers present in the public catalog at review time. The live catalog remains an availability intersection only. A new identifier or metadata change cannot alter the built-in route, protocol, capability, limit, or disclosure.

The globally unique wire revisions are:

- OpenAI Responses revision 1;
- OpenAI-compatible Chat Completions revision 2;
- Anthropic Messages revision 3; and
- Google Gemini `streamGenerateContent` revision 4.

The reviewed Zen groups are:

- **Responses (26):** the 20 live GPT entries, Grok 4.5, Grok 4.6, Grok Build 0.1, Muse Spark 1.2, and the two Muse contributor-free entries;
- **Anthropic Messages (14):** the 12 live Claude entries plus Qwen3.5 Plus and Qwen3.6 Plus;
- **Gemini (7):** Gemini 3 Flash, 3.1 Pro, 3.5 Flash, 3.5 Flash Lite, 3.6 Flash, 3.7 Flash, and 3.8 Flash; and
- **Chat Completions (19):** the live DeepSeek, GLM, MiniMax, Kimi, Big Pickle, MiMo free, Ling free, Nemotron free, and Laguna free entries.

All entries retain Morons' conservative 96,000 estimated-input and 32,000 output-token limits. Text input, text output, reasoning, and function tools are admitted for every entry. Images are admitted only where the reviewed OpenCode runtime metadata declares image input and the matching bounded protocol conversion is tested. Audio, video, and PDF inputs remain outside the Morons surface even where an upstream model accepts them.

### Fixed routes and credential scoping

Production may send Zen inference only to:

- `https://opencode.ai/zen/v1/responses` with sensitive bearer authorization;
- `https://opencode.ai/zen/v1/chat/completions` with sensitive bearer authorization;
- `https://opencode.ai/zen/v1/messages` with sensitive `x-api-key`, fixed `anthropic-version: 2023-06-01`, and no bearer authorization; or
- `https://opencode.ai/zen/v1/models/{reviewed-model-id}:streamGenerateContent?alt=sse` with sensitive `x-goog-api-key` and neither bearer authorization nor `x-api-key`.

The Gemini path is constructed only from the model identifier already selected from the immutable manifest. Catalog content, repository state, provider output, and configuration cannot supply that identifier or alter the fixed origin, prefix, method suffix, or query. Every inference family retains the stable derived `x-opencode-session` header and bounded Morons user agent. Catalog requests receive no credential or affinity header.

Responses, Chat Completions, and Anthropic Messages retain their reviewed protocol boundaries. Chat Completions additionally accepts internally consistent cumulative usage snapshots: input totals remain stable while output, total, cache, and reasoning counters may only increase. This matches OpenCode's latest-usage parser and the reviewed Zen DeepSeek stream behavior without permitting decreasing or contradictory accounting.

### Gemini request and response boundary

Revision 4 follows OpenCode's native Gemini adapter rather than translating Gemini models through an OpenAI-shaped route. Requests use:

- ordered `contents` with only `user` and `model` roles;
- one text-only `systemInstruction` assembled from leading developer context;
- normalized images as bounded base64 `inlineData` parts;
- `functionCall` and `functionResponse` parts with strict-object arguments and name-bound results;
- one bounded `functionDeclarations` tool group whose schemas are sanitized and projected into Gemini's reviewed schema dialect; and
- a bounded `generationConfig.maxOutputTokens`.

Morons does not request Google Search, URL context, code execution, response schemas, file references, safety-policy overrides, cached-content resources, audio, video, PDF, or generated media.

The decoder accepts strict bounded SSE JSON for one candidate, visible text deltas, thinking text, thought signatures, function calls, terminal finish reasons and bounded finish messages, response/model identity and a validated protobuf creation timestamp, safety ratings, the recorded default `standard` service tier, cumulative usage, and at most one terminal bounded `{"type":"ping","cost":...}` OpenCode Zen accounting trailer. The reviewed finish-message and service-tier shapes come from OpenCode's pinned Gemini stream recordings, and the cost trailer comes from `packages/console/app/src/routes/zen/util/provider/provider.ts`; OpenCode's normalized parser strips fields outside its smaller retained schema, while Morons must represent each accepted wire field explicitly because it rejects unknown fields. The decoder rejects unknown content parts, multiple candidates, unrequested grounding/citations/log probabilities, provider-executed tools, malformed function arguments, contradictory identity or usage, unreviewed service tiers, unsafe/incomplete terminal states, and all existing stream/resource-limit violations.

Gemini's visible candidate token count excludes thought tokens, so normalized output usage is their checked sum. Prompt usage includes the cached subset. Total usage must equal normalized input plus output. Thinking text is bounded and suppressed. A function call's thought signature is bounded, redacted in diagnostics, retained only in trusted memory for the active tool loop, and replayed on that function-call part as OpenCode's adapter requires. It is never canonical transcript content, SQLite state, an authorization value, or cross-run memory. Restart recovery interrupts the run rather than replaying a lost signature.

Gemini does not require a provider-generated function-call identifier. Morons derives a bounded opaque call identifier from the provider response ID and call index for internal validation. The canonical tool-call identity remains Morons-owned; the response signature is attached to the corresponding committed call only while the live run continues.

### Data-use disclosure

The current Zen privacy documentation states that models are not used for training and use zero retention except for named exceptions. Morons freezes the following reviewed classifications:

- the 20 GPT entries: no training, retention up to 30 days under the OpenAI API exception;
- the 12 Claude entries: no training, retention up to 30 days under the Anthropic API exception;
- Big Pickle, MiMo-V2.5 Free, Ling 3.0 Flash Fin Free, both Nemotron free entries, and both Muse contributor-free entries: prompts/completions may be used for model or product improvement and retention is not zero; and
- every other reviewed Zen entry: no training and zero retention under the published blanket policy.

These are disclosures, not guarantees by Morons. Provider terms remain authoritative and may change. Training-eligible and non-ZDR entries remain selectable by default; future training-blocking and ZDR-required settings remain separate opt-in restrictions.

Application protocol v36 adds the Gemini protocol label to typed model summaries. Persistence schema v25, context policy v4, and tool catalog/limits v8 remain unchanged. Durable runs already bind the exact service, model, and globally unique protocol revision without persisting provider enum layouts or ephemeral continuation metadata.

## Consequences

- The reviewed Zen picker can expose the intersection of all 66 frozen entries and the current public catalog.
- Zen root runs, compaction, inherited children, and exactly selected children use one pinned protocol without fallback.
- OpenCode Go remains the separately reviewed 35-entry manifest from ADR 0020; the same pinned-source audit confirmed its existing wire behavior without changing its routes.
- Gemini adds one isolated request/decoder module and one fixed credential-header convention.
- A stale catalog entry or changed upstream implementation may still fail. Morons reports that exact failure and never guesses another protocol or substitutes a model.
- New models, route migrations, capability changes, protocol fields, and data-use changes require another reviewed manifest revision.
