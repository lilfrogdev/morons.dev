# ADR 0020: Complete OpenCode Go model and protocol support

## Status

Accepted

## Context

OpenCode Go's public `https://opencode.ai/zen/go/v1/models` catalog exposed 35 model identifiers during the 2026-09-04 review. Morons' built-in manifest admitted only Go GPT 5.6 Luna, Grok 4.6, and GLM-5.3-Flash. The missing models cannot all be sent through one compatible endpoint: OpenCode's current Go documentation assigns models to OpenAI Responses, OpenAI-compatible Chat Completions, or Anthropic Messages.

The public model catalog carries only identifiers. It does not carry a trustworthy route, protocol, capability, limit, or data-use classification. Allowing a catalog response to select those properties would let remote untrusted data redirect credentialed traffic or enlarge Morons' reviewed capability surface. Conversely, keeping a three-model manifest prevents users from selecting models included in their Go subscription.

The current model list and privacy table cover 27 of the 35 live identifiers. The endpoint table additionally retains an Anthropic Messages route for `minimax-m2.5`, so 28 identifiers have current first-party routing documentation. Seven older identifiers remain in the live catalog but no longer appear in the endpoint or privacy tables: `glm-5`, `grok-4.5`, `hy3-preview`, `kimi-k2.5`, `mimo-v2-omni`, `mimo-v2-pro`, and `qwen3.5-plus`. Their last documented protocol mapping remains reviewable. Current training and retention policy is undocumented for those seven identifiers and for `minimax-m2.5`, and must not be guessed.

## Decision

Morons carries a reviewed built-in snapshot for all 35 Go identifiers present in the public catalog at review time. The remote catalog continues only to mark these exact entries available or unavailable. It cannot add an identifier or change its route, protocol, capability, limit, or disclosure.

The manifest assigns globally unique protocol revisions:

- OpenAI Responses revision 1;
- OpenAI-compatible Chat Completions revision 2; and
- Anthropic Messages revision 3.

The reviewed Go Responses group is `gpt-5.6-luna`, `grok-4.5`, `grok-4.6`, `muse-spark-1.2-contributor`, and `muse-spark-1.3-contributor`. The reviewed Go Anthropic Messages group is `minimax-m2.5`, `minimax-m2.7`, `minimax-m3`, `qwen3.6-plus`, `qwen3.7-max`, `qwen3.7-plus`, `qwen3.8-flash`, and `qwen3.8-max`. Every other reviewed Go identifier uses Chat Completions revision 2. Zen has no admitted Anthropic Messages route under this decision.

All Go requests retain the conservative Morons limits of 96,000 estimated input tokens and 32,000 maximum output tokens even when upstream metadata advertises larger limits. Every admitted model supports text input, text output, reasoning, and function tools in the reviewed surface. Image input is enabled only for model/protocol pairs whose current model metadata documents an image modality and whose request conversion is covered by bounded fixtures. Audio, video, and PDF provider modalities are not admitted.

### Anthropic Messages boundary

Production adds only the fixed `https://opencode.ai/zen/go/v1/messages` route. The server sends the same generation-bound OpenCode credential in a sensitive `x-api-key` header, sends fixed `anthropic-version: 2023-06-01`, and does not send `Authorization` on that route. TLS, redirect denial, deadlines, cancellation, response and error limits, no-retry behavior, and stable `x-opencode-session` affinity remain unchanged.

Provider-neutral input is lowered into a bounded Messages request:

- leading developer messages become one ordered `system` string;
- user and assistant text become typed content blocks;
- normalized images become base64 image-source blocks only for reviewed vision models;
- adjacent assistant text and function calls become one assistant content sequence;
- strict-object function arguments become `tool_use.input`;
- consecutive function results become user `tool_result` blocks bound to exact call IDs;
- tools use `name`, `description`, and `input_schema`; and
- `model`, `max_tokens`, and `stream` are explicit.

Responses reasoning-continuation items, arbitrary beta headers, provider-selected tools, prompt-cache controls, compatibility configuration, and repository-controlled request fields are absent.

The revision-3 decoder accepts the ordered Messages stream state machine: `message_start`, sequential content-block start/delta/stop groups, `message_delta`, and `message_stop`, with bounded `ping` records. It normalizes text deltas and complete `tool_use` blocks into the existing provider-neutral outcome. It accumulates partial tool JSON before strict duplicate-key parsing, ignores but independently bounds thinking and signature deltas, validates exact model and message identity, validates stop reason and usage against reviewed limits, and fails closed on malformed order, unknown structures, truncation, contradictory output, oversized data, or streamed errors. Initial usage may be zero or partial where the gateway supplies a monotonic cumulative value in `message_delta`. Cached-read and cache-creation input are included in total input while remaining separately disclosed in normalized usage.

### Chat Completions compatibility

Revision 2 now supports bounded normalized image content for reviewed Go vision models. DeepSeek assistant replay includes an empty `reasoning_content` field when no portable reasoning content is retained, matching the documented compatibility requirement without persisting hidden reasoning. Current compatible gateway variants may emit nullable cache details, top-level cache hit/miss counts, cache-write counts, bounded `reasoning_details`, a no-op terminal usage choice, or a repeated usage envelope that enriches only cache/reasoning breakdowns while preserving primary totals. The decoder normalizes those variants but rejects contradictory totals, decreasing details, unsupported nonzero audio or prediction use, and visible duplicate output. Reasoning remains bounded, ignored, and unavailable as cross-turn continuation for Chat Completions and Anthropic Messages.

### Data-use disclosure

Current documented Go entries use the exact published classification:

- Grok 4.6 and GPT 5.6 Luna: prompts and completions are not used for training; retention is up to 30 days;
- Muse Spark contributor entries: prompts and completions may be used for training; retention is documented only as not zero-data-retention; and
- the other entries in the current privacy table: prompts and completions are not used for training and retention is zero days, including the provider's time-qualified DeepSeek ZDR agreement.

The seven live catalog-only identifiers and the routed but privacy-omitted `minimax-m2.5` display **not documented** for both training and retention. They remain selectable by default, like the reviewed contributor entries, but the UI does not imply a more favorable policy. This decision adds truthful disclosure variants; it does not implement or enable a data-use restriction setting.

Application protocol v35 adds Anthropic Messages and the additional training/retention disclosure variants to typed model summaries. Persistence schema v25 is unchanged because durable runs already store the globally unique protocol revision and exact service/model pair without serializing protocol DTO enums or data-use labels.

### Review evidence

The routing and disclosure review uses OpenCode's current Go documentation and public `/v1/models` response as the first-party authorities. At review time, the catalog returned the exact 35 identifiers frozen in the manifest test, while the documentation supplied 28 current route rows and 27 current privacy rows.

The latest first-party historical route rows are retained only for live catalog identifiers omitted from the current endpoint table:

- OpenCode commit `6a7ca45ae6dc9b144f9e86d489fd6abb628a9884` documents Chat Completions for `glm-5`, `kimi-k2.5`, `mimo-v2-omni`, `mimo-v2-pro`, and `qwen3.5-plus`;
- commit `bcbc1dba22f1524dbc2c8ade6b3f87d27a30da57` documents Chat Completions for `hy3-preview`; and
- commit `c7af47f9ed3b70d7e1e5cf4b37c6d8ef6f83b3bc` is the latest documented migration of `grok-4.5` to Responses.

Third-party catalogs and installed client data may corroborate modality and protocol research but are not routing, admission, or policy authorities. They cannot override these pinned first-party rows, add a live identifier, or turn missing privacy documentation into a favorable disclosure.

## Consequences

- Every identifier in the reviewed 2026-09-04 Go catalog snapshot can appear in `/model` and `/settings` when the public catalog reports it available.
- Root runs, compaction, inherited children, and exactly selected children use each model's pinned protocol without fallback.
- Training-eligible, non-ZDR, and undocumented-policy models are visible with exact disclosure rather than silently omitted.
- A stale or inconsistent upstream catalog may mark a reviewed but unusable model available. Morons reports the exact provider failure and does not retry through another model or protocol.
- New Go identifiers, protocol migrations, data-use changes, and newly documented capabilities require another reviewed manifest change; the public catalog alone never activates them.
- The Messages decoder and image conversions increase provider-adapter surface area, but keep it isolated from persistence, application logic, credentials, and terminal presentation.
