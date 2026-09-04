# ADR 0018: Global subagent model setting

- Status: Accepted
- Date: 2026-09-04

## Context

ADR 0013 deliberately launched every `task` child with the parent's exact reviewed OpenCode service and model. That is a safe default, but it prevents a user from assigning a faster or more economical model to focused child work and prevents useful cross-family combinations such as a Zen GPT 5.6 Sol parent with Go GLM-5.3-Flash children.

Model access is not model preference. Repository files, skills, prompts, and provider catalog ordering must not choose global billing or routing policy. A configured child model must remain an exact reviewed pair, be stable for a task batch, use its own reviewed protocol and limits, and be disclosed in the result. If that pair becomes unavailable or unauthorized, Morons must fail clearly rather than silently spend against another model.

## Decision

Morons adds a typed, server-authoritative global application setting named `subagent_model`. Its closed protocol shape is either:

- `inherit_parent`, the default when no setting has ever been stored; or
- one exact OpenCode service and model identifier.

`/settings` refreshes the server-authoritative value and opens an extensible typed settings dialog rather than accepting free-form configuration text. Its initial row is **Subagent model**. Enter opens a bounded searchable picker containing **Inherit parent** and currently available models from Morons' reviewed manifest intersected with the provider catalog. The picker discloses the selected model's reviewed protocol revision and data-use policy. The saved exact setting remains visible as unavailable if it is absent from the current client catalog; the UI does not silently rewrite it.

The server validates every exact mutation against the built-in reviewed model manifest and requires text input, text output, and tool-call capability. Repository-controlled configuration cannot set or override this value. Application protocol v34 carries typed settings query and mutation messages. Credential bytes and status remain outside the settings payload.

### Persistence and mutation semantics

Persistence schema v25 adds an append-only `subagent_model_selections` table and mutation operation 16. Each row records a domain-separated fingerprint of either inheritance or the exact service/model pair, along with the global logical acceptance sequence and timestamp. Exact mutation retries return the existing value; conflicting reuse fails. The absence of a row deterministically means **Inherit parent**. Startup validates setting shape, model-identifier syntax, fingerprints, mutation-registry linkage, and sequence integrity.

The settings table records preference, not provider catalog availability, credentials, or repository state. Those remain independently reviewed or checked at their existing boundaries.

### Task pinning and routing

Immediately before a prepared `task` operation is marked dispatched, the supervisor reads the current global setting once:

- inheritance copies the accepted parent run's durable service, model, protocol revision, limits, and credential generation; or
- an exact setting resolves the built-in reviewed model and pins its service, model, protocol revision, and limits while retaining the parent run's accepted credential generation.

That in-memory value is immutable for all children and turns in the batch. A concurrent settings change affects only a later task resolution. It does not change an already running child and does not alter the main session model.

A missing reviewed model, incompatible capability, changed credential generation, authentication failure, entitlement failure, or provider unavailability returns a bounded explicit task failure. There is no fallback to the parent, global default model, catalog order, or another provider. Existing no-retry and uncertain-effect rules continue to apply after dispatch.

Each completed child result stores and presents the selected service, model identifier, and globally unique provider protocol revision. Historical task results without this additive disclosure remain decodable. Child conversation identifiers continue to derive from the parent session, parent task call, and child index; changing the selected child model does not collapse root or sibling identities.

The currently supported exact setting uses the existing OpenCode credential boundary. The tagged setting type can gain a separately reviewed provider variant in a later protocol revision; this decision does not authorize a new credential source or network origin.

## Consequences

- **Inherit parent** remains the out-of-the-box behavior.
- Users can deliberately pair different reviewed model families, OpenCode services, and supported wire protocols for parent and child work.
- A setting is global across sessions, clients, and restarts but independent from the global main-model default.
- Settings changes do not mutate historical runs or already pinned task batches.
- Task results make model routing inspectable without exposing credentials or provider payloads.
- Schema and application protocol versions advance; tool catalog/limits versions remain 8 because the callable `task` input contract and execution limits do not change.

## Alternatives rejected

- **Per-session or per-call free-form model strings:** duplicates policy, weakens validation, and lets prompt or repository content steer billing.
- **Automatic cheapest/available fallback:** makes spending and behavior nondeterministic and violates explicit selection.
- **Reuse the global main-model default:** couples independent preferences and causes `/model` to change child routing unexpectedly.
- **Persist catalog availability or credentials with settings:** confuses transient provider state with durable owner preference and crosses credential boundaries.
