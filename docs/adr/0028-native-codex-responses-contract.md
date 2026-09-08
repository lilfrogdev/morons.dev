# ADR 0028: Native Codex Responses contract

## Status

Accepted for staged implementation, 2026-09-06. The concrete adapter was implemented and tested before application admission. [ADR 0030](0030-native-provider-and-task-bindings.md) now supplies the explicit native run/child/maintenance integration required by [ADR 0019](0019-openai-subscription-authentication-and-hosted-search.md), using the durable policy controls in [ADR 0029](0029-provider-admission-and-data-use-policy.md). This does not authorize live requests or establish subscription interoperability.

[ADR 0035](0035-reviewed-native-model-expansion.md) subsequently extends the exact full-Responses manifest to Astra, Sol, Luna, Terra and Daybreak Blue after additional Pi adapter review. The official Codex client's Lite preference below is not evidence that the backend requires Lite exclusively. No Lite dialect, automatic model admission or historical request-byte change is introduced.

## Reviewed evidence

- OpenAI [Codex models](https://developers.openai.com/codex/models), fetched 2026-09-06, states that ChatGPT-authenticated GPT-5.4 and GPT-5.4 Mini retire August 31, 2026. API availability is not subscription availability. GPT-5.5 remains listed as a previous-generation option.
- Official Codex source at `459a79eb85400af759e9220c7bafb4429ae07516`: `codex-rs/codex-api/src/common.rs` (ResponsesApiRequest), `core/src/client.rs` (request construction), `codex-api/src/endpoint/responses.rs`, `codex-api/src/requests/headers.rs`, `codex-api/src/sse/responses.rs`, and `models-manager/models.json`. GPT-5.5 uses full Responses, supports text/images, function tools and medium reasoning; its default context is 272,000 tokens. GPT-5.6 Sol/Terra/Luna select Responses Lite in that manifest and are outside this contract.
- OpenCode source `5cf9f517cfec3ef68d3e68a12a6a4b3163947f44`, `packages/opencode/src/plugin/openai/codex.ts`, corroborates the fixed backend route, bearer/account headers and GPT-5.5 subscription routing. Its automatic future-model admission, zero-price projections, retry/recovery and alternative transports are not adopted.
- [OpenAI enterprise privacy](https://openai.com/enterprise-privacy/) documents plan/workspace-dependent training and retention. Token claims, `store:false`, and account presence do not establish favorable settings.

These are reviewed public source snapshots, not imported instructions, dependencies, credentials, remote model-admission authority, a Morons support agreement or live interoperability proof.

## Fixed adapter

Only `https://chatgpt.com/backend-api/codex/responses` is admitted by this adapter. Initially it reviews only `gpt-5.5`, independently of OpenCode's model/service types. It uses the existing web-PKI HTTP stack, no ambient proxy/certificate override, compression, websocket, alternate endpoint, catalog lookup, hosted tool, automatic retry or model fallback.

Requests carry fixed `originator: morons`, Morons user-agent, JSON/SSE content types and sensitive bearer/ChatGPT-Account-Id headers from the exact OpenAI credential lease. Conversation/session/thread/cache identifiers are source-separated hashes of the supplied conversation and credential generation; per-request identity also binds the run and request sequence. No directory, prompt or raw account enters these identifiers. The lease stays held through response headers and is then released, allowing credential changes without holding a lock for the entire stream.

The body includes explicit Morons `instructions`, full input projection, reviewed function tools, `tool_choice:auto`, `parallel_tool_calls:false`, `reasoning.effort:medium`, `text.verbosity:low`, `include:[reasoning.encrypted_content]`, `store:false`, `stream:true` and a conversation-scoped `prompt_cache_key`. It does not add service tier, automatic context management, remote memory, previous_response_id or internal Codex metadata. Project guidance remains separate untrusted developer input, not promoted into the core instructions.

Native ResponsesApiRequest omits `max_output_tokens`. Morons must not send the OpenCode body unchanged, silently claim a provider-enforced output allowance or switch to API-key billing. The existing 96,000-input and 32,000-output ceilings remain conservative **local acceptance limits**; the caller can choose a lower local output limit. Output/usage over that limit rejects, and transport/SSE/text/tool limits and deadlines terminate local consumption. The backend may already have generated or charged more; a local cap is not a spending cap. Neither deadline nor cancellation retracts remote work. This distinction must be surfaced when models are admitted to the application.

A mutable, non-clonable ephemeral turn object binds the provider instance's private registration and exact conversation, run, model and OpenAI identity generation. Prepared bytes bind its private registration and one request sequence, preventing reuse across turns or replay of an old prepared request after success. It retains only the first bounded sensitive `x-codex-turn-state` for that turn, never across runs or in SQLite/context/UI. Conflicting or malformed routing state fails closed. Caller-supplied opaque headers are not accepted. Reasoning input must match a bounded private fingerprint set from that turn's most recent completed response; this does not permit foreign or edited opaque reasoning, nor persistence of hidden content across runs. A dispatched attempt that is cancelled, dropped, malformed or otherwise uncertain poisons the turn; it cannot retry automatically. Credential refresh retains ADR0019's independent durable no-replay rules.

The existing strict Responses SSE state machine and input/tool/image validators are reused without relaxing OpenCode behavior. Exact model/lifecycle/sequence, complete attributed output, usage arithmetic, terminal state, bounded opaque reasoning and UTF-8 remain required. Native bodies and provider headers do not select capabilities or destinations.

## Policy and application gates

A concrete two-boolean data-use restriction value permits training-restricted requests only for reviewed NotUsed metadata and retention-restricted requests only for reviewed None retention. Unknown/account-controlled values fail either restrictive setting. Both switches default off. The native request and dispatch boundaries enforce the supplied policy; the server must still durably own and consistently supply the current policy at every root, child and maintenance boundary before exposing these models. A caller-provided library value is not a substitute for that application enforcement.

No native model is inserted as a third OpenCode service. Before application admission, generalize explicit provider/model identity, preserve historical OpenCode encodings and acceptance evidence, bind cross-provider children to their own credential generation, and bind maintenance to its trigger. No UI login automatically selects a model. No owner browser login, retained-state upgrade or live inference is authorized by deterministic adapter tests.

## Validation

Synthetic loopback tests cover exact bodies/headers, images/tools/phase/opaque reasoning, local limit and policy rejection, sequence/turn/generation binding, routing-state isolation, cancellation and dropped attempts, no retry/redirect/fallback, malformed/truncated/oversized streams and bounded errors. Existing OpenCode regressions must remain green. Hosted platform CI, live subscription behavior and native/signed release qualification remain separate gates.
