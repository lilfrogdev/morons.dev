# ADR 0014: Global default model and searchable selector

## Status

Accepted

## Context

The initial terminal client cycles every available model with `Tab` and otherwise selects the first available catalog entry after startup. This makes ordinary skill completion compete with model selection and makes users repeatedly traverse models they do not intend to use. Restoring each session's historical model would also make model choice session-local, while the desired behavior is one user preference shared by every session.

Model choice affects provider routing and data-use disclosure, but it is not authorization evidence. ADRs 0004 through 0006 require every run to carry an exact reviewed service and model, with authoritative server validation and durable run attribution.

## Decision

Morons provides `/model [search]` in the session message editor. The command opens a bounded searchable selector over models that the authenticated server catalog marks available. The current model is identified, typing filters by service, identifier, and display name, Up and Down move through matching entries, Enter confirms, and Escape cancels. The selected entry continues to show its reviewed training and retention disclosure. `Tab` no longer cycles models and remains available for skill completion.

One default model applies across the application rather than belonging to a session. Opening or creating a session does not restore that session's historical model or otherwise replace the global choice.

Confirming a selector entry commits an idempotent local-owner application mutation containing a stable mutation request identifier, exact OpenCode service, and exact model identifier. The server accepts only a model in the reviewed built-in manifest and stores one bounded canonical default-selection fact in SQLite. Exact retries return the same selection, conflicting reuse fails, and the history has an explicit count limit.

The effective default is the service and model from whichever occurred later in the server's logical sequence: an explicit default-selection fact or an accepted top-level run. This preserves the last model actually used when runs are submitted by an older client or another attached client. A selector confirmation becomes the default immediately even if no run follows.

At startup, explicit refresh, and before opening a newly created or existing session, the client queries the effective default over authenticated IPC. This lets another authenticated local client's later selection or accepted run become the next session's default without changing a model underneath an already open composer. The client selects that exact pair only if the current sanitized server catalog marks it available. If the saved pair is unavailable or no longer present, the client uses another available reviewed model and reports the fallback without silently changing the durable preference. An unavailable fallback cannot enlarge the reviewed catalog or bypass server validation.

Every submitted run still includes its exact service and model. The server validates and records that pair independently of default state. Default state does not select an endpoint, credential, capability, provider header, or model outside the reviewed manifest, and it is never sent to a provider by itself.

The application protocol becomes version 31 and persistence schema becomes version 24. Schema 24 extends the mutation-operation registry and adds canonical default-model selection facts; no provider call, session event, filesystem effect, or credential operation occurs during selection.

## Validation

Tests cover:

- strict protocol shapes and exact client response correlation;
- reviewed-model validation, idempotent retry, conflicting reuse, and persistence across restart;
- logical ordering between explicit selections and accepted runs;
- migration from schema 23 and canonical fingerprint/integrity validation;
- searchable modal rendering, filtering, keyboard selection, cancellation, and bounded input;
- global selection across session open and close;
- unavailable-default fallback; and
- proof that `Tab` no longer changes the selected model.

## Consequences

- Users select models deliberately instead of traversing the catalog with `Tab`.
- New and existing sessions consistently start from the user's latest global choice.
- A small durable preference mutation and protocol query are added, while per-run model selection remains authoritative.
- Multiple local-owner clients may change the same global default; logical ordering gives a deterministic result on the next query or refresh.
- Historical selection facts consume bounded storage but do not belong to, or get deleted with, any session.
