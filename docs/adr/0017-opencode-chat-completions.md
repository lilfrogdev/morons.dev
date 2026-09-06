# ADR 0017: Bounded OpenCode Chat Completions

## Status

Accepted; expanded by ADR 0020

## Context

ADR 0004 admitted only OpenAI Responses-compatible OpenCode models. OpenCode Go now documents `glm-5.3-flash` at `https://opencode.ai/zen/go/v1/chat/completions`, using the OpenAI-compatible Chat Completions protocol. It cannot be truthfully routed through the existing Responses endpoint or decoder.

Morons must preserve fixed reviewed routing, server-owned credentials, stable `x-opencode-session` affinity, no post-dispatch retry, bounded streaming, provider-neutral run outcomes, and the existing tool loop. A remote catalog must not choose a protocol or endpoint.

## Decision

The reviewed model manifest assigns every service/model pair one internal wire protocol and globally unique protocol revision:

- Responses revision 1; or
- Chat Completions revision 2.

Application protocol version 33 exposes this pair as `protocol` and `protocol_revision` in model summaries instead of describing every revision as a Responses revision. Durable run records continue storing the globally unique revision number; persistence schema 24 is unchanged.

This change initially admitted exactly OpenCode Go `glm-5.3-flash` through Chat Completions revision 2. It was text-only in the initial reviewed capability surface, supported reasoning output and function tools, did not support portable reasoning continuation, used the conservative 96,000 input and 32,000 output token limits, and carried the policy documented at that review time. ADR 0020 reviews the expanded Go catalog, enables bounded image input for documented Chat Completions vision models, and updates current data-use disclosures while preserving revision 2 and the same limits.

Production routing adds only `https://opencode.ai/zen/go/v1/chat/completions`. Zen Chat Completions has no admitted route. Catalog, credential, TLS, redirect, deadline, cancellation, response-header, error-body, session-affinity, and no-retry behavior remain the ADR 0004 boundary.

The provider-neutral request is converted into typed Chat Completions messages:

- developer instructions become `system` messages because this model does not support the OpenAI developer role;
- user and assistant text remain ordered messages;
- consecutive function calls become one assistant `tool_calls` message;
- function results become `tool` messages bound to the exact call ID;
- tools use the nested OpenAI-compatible function schema;
- `max_tokens`, streaming, and streaming usage are explicit; and
- parallel tool calls, Responses reasoning items, store, repository configuration, and arbitrary compatibility options are absent.

ADR 0020 later adds bounded normalized image parts for reviewed Go vision models and the explicit empty DeepSeek `reasoning_content` compatibility field. Neither addition permits catalog- or repository-controlled options.

The streaming decoder uses the existing bounded SSE framing and strict duplicate-key JSON parser. It bounds object depth, nodes, fields, collection sizes, keys, identifiers, deltas, accumulated visible text, ignored reasoning text, tool calls, arguments, token usage, and total stream bytes. It requires one response identity and exact model, one choice index, assistant roles, valid terminal reasons, complete strict-object tool arguments, usage consistent with reviewed limits, and a done marker.

Observed OpenCode/Z.AI compatibility remains explicit and narrow:

- `object`, request ID, service tier, and system fingerprint metadata are optional but bounded;
- repeated `assistant` role deltas are accepted, while any other role is rejected;
- `reasoning` and `reasoning_content` deltas are bounded but not persisted or replayed;
- optional empty web-search metadata is accepted although Morons does not request hosted search;
- whitespace and repeated `[DONE]` markers are accepted;
- after terminal completion, only repeated done markers, a same-identity empty Chat chunk, or OpenCode's exact `{ "choices": [], "cost": <bounded decimal> }` trailer is accepted; and
- cost may be a JSON number or short decimal string, is bounded and validated, and is ignored for billing and usage accounting.

Any post-terminal text, tool call, usage, nonempty choices, nonnumeric cost, error, unknown field, or malformed structure fails closed. Debug builds may emit only content-free decoder stages and unknown-field fingerprints; release builds never log provider payload data.

Chat outcomes map into the existing provider-neutral assistant, tool-call, usage, delta, cancellation, persistence, and uncertainty paths. Compaction and bounded task children use the same reviewed model protocol automatically when selected. Chat reasoning is not inserted into later turns.

## Consequences

- Users can select Go `glm-5.3-flash` for root runs, tool loops, compaction, and current inherited-model task children.
- Main and future subagent models may use different wire protocols without catalog-controlled routing.
- The model catalog wire contract becomes protocol-accurate and application protocol v32 clients fail the normal version handshake.
- Existing Responses models and durable schema remain unchanged.
- Ignoring bounded reasoning text may reduce continuity compared with provider-specific preserved-thinking modes, but avoids inventing an unreviewed cross-turn contract.
- OpenCode cost metadata is not authoritative local accounting; subscription limits and billing remain external.
