# ADR 0029: Provider admission and data-use policy

## Status

Accepted for staged implementation. Native model admission remains closed until the complete provider/credential binding and policy path is qualified. This decision does not authorize real authentication, requests, retained-state migration or merging the draft.

The first implementation increment supplies durable policy controls and enforcement for all currently enabled OpenCode selection/dispatch surfaces (IPC 41, SQLite 29). Native provider/model/credential generalization and durable cross-provider task bindings are still required before enabling ChatGPT in the application.

## Boundary, before implementation

OpenCode Zen/Go and native ChatGPT are distinct service identities. Generic application and persistence model selection must not encode ChatGPT as an OpenCode billing service or use an OpenCode key. Preserve historical Zen=1/Go=2 records, immutable acceptance evidence, request encodings and context digests. Each root and maintenance job uses its selected service's credential generation. A task batch must durably capture its own reviewed model, protocol and credential generation before its outer effect is dispatched; cross-provider children cannot reuse a parent's unrelated counter. New native turns belong to individual root, child or compaction executions; opaque state never crosses these lifetimes or restart.

Introduce two independent owner-controlled SQLite settings: Block training use and Require zero data retention. Both default off. Mutations use authenticated IPC, a bounded idempotent owner ledger and the existing single storage worker. The reviewed manifest alone supplies data-use facts: training restriction requires NotUsed; retention restriction requires None. Unknown/account-controlled policy fails restrictive settings. No inference from a JWT, subscription, login, catalog or store:false. Changing policy never silently clears or substitutes a selected model.

Enforce policy at model selection, run/explicit compaction acceptance and every provider dispatch admission, including child turns and automatic foreground/background compaction. A blocked model may remain visible and selected with a clear explanation, but cannot receive a newly admitted request. Data-policy rejection is a distinct fixed application/run/tool failure, not an entitlement error or an uncertain external effect. Preserve existing known-terminal handling if rejection happens between turns or after preparation.

Dispatch admission is the linearization boundary: the storage worker checks the current policy before authorizing the dispatch, serialized with settings mutations. A later policy change cannot recall already admitted or transmitted work, refund use, roll back tools, or guarantee that a remote model stopped. Credential leases retain their separate provider-scoped coordination. Do not hold a global policy lock for an entire response stream, retry a rejected/uncertain effect, or treat client-supplied booleans as authority. Native transport receives the policy authorized by this server boundary, not arbitrary IPC policy values.

Background jobs additionally pin the current policy sequence at preparation. A change invalidates stale Prepared/Ready work before dispatch or installation; do not reroute it. Dispatched work retains supervision through completion/cancellation. Historical jobs with the original default policy retain their original binding digest; new nondefault policy bindings are source-bound and validated against the immutable policy ledger.

The terminal settings view exposes the independent switches and explains blocked unknown policy. Native model metadata must disclose that its output limit is local validation, not a backend generation/spending cap. Login never selects a model automatically. Only after these guards, explicit identities and synthetic end-to-end tests are complete may the picker expose the reviewed native GPT-5.5 model.

## Policy ledger and compatibility

SQLite 29 adds at most 10,000 immutable policy choices using owner mutation kind 17, exact prior-policy sequence checks and source-bound fingerprints. Returning a prior mutation receipt does not reapply it. Inconsistent ledger/mutation facts, sequence chains and fingerprints fail closed on startup and after external data-version changes. Policy changes do not touch credentials, selected directories, transcripts or default-model choices. Policy rejection uses run failure 12; the relevant canonical/provider/projection constraints are migrated explicitly. Old failure encodings, accepted receipts and model/body projections remain unchanged.

Maintenance pins a `data_use_sequence`. Zero denotes the historical default policy and retains the original binding bytes; nonzero sequences extend the binding and must be the latest policy preceding the job. Stale Prepared work is cancelled before dispatch; stale Ready work is discarded at a boundary/recovery. Changing settings does not pretend a Dispatched task has drained. Test migration fixtures restore the exact v28 affected tables before applying older fixture downgrades; production migration constraints are not relaxed.

The client uses a closed, sequence-checked settings mutation and validates acknowledgement scope. It does not optimistically change policy, silently replace a blocked model or let an older acknowledgement clear newer policy presentation. Model pickers display blocked entries and preserve the selected model/draft. Server checks remain authoritative if another client changes settings before the view refreshes.

## Qualification

Use disposable stores and synthetic transports. Cover the four policy combinations and every reviewed metadata category; idempotency/conflicts/restart/populated migration/corruption; policy changes between acceptance, preparation and dispatch; cross-provider credential generation/account changes; root/child/foreground/background policy enforcement and stale Ready disposal; exact native tools/images/continuations/cancellation/recovery; unchanged historical OpenCode evidence and selected-directory effects. Run formatting, all-target checks, warnings-denied debug/release Clippy, locked tests, dependency policy and fresh exact-head platform/security CI. Owner browser sign-in and a separately approved live budget follow, not precede, these gates.
