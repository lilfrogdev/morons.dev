# ADR 0026: Bounded background-compaction maintenance

- Status: Accepted — default-on supervised runtime with owner opt-out and first-request installation
- Date: 2026-09-05
- Builds on: [ADR 0012](0012-trusted-local-direct-workspace-mvp.md), [ADR 0022](0022-release-context-and-provider-hardening.md), [ADR 0023](0023-context-observation-and-request-preparation.md), [ADR 0025](0025-project-guidance-and-prompt-led-delegation.md)

## Context

Originally, compaction was synchronous and belonged to an active top-level run. Preparing a summary earlier can hide some provider latency, but launching an untracked task would violate Morons' durable effect accounting, restart policy, bounded execution and session lifetime rules. Background inference also incurs potential charges even when its output is never installed.

Background compaction does not require native OpenAI subscription authentication. Qualification of the existing tool loop and focused reliability repairs precede background execution implementation. Run-specific QA evidence stays outside the source repository; reproducible regression tests and architectural decisions belong here.

OMP at `0fcdbb30f6b532365d9b7ec7e38c86ae2fae91ec` is a reference, not a dependency or a complete audited design:

- [`session-maintenance.ts`](https://github.com/can1357/oh-my-pi/blob/0fcdbb30f6b532365d9b7ec7e38c86ae2fae91ec/packages/coding-agent/src/session/session-maintenance.ts) prepares off a branch snapshot, holds an armed result, checks it at application time, and exposes idle/running/armed state.
- [`speculation-lead.ts`](https://github.com/can1357/oh-my-pi/blob/0fcdbb30f6b532365d9b7ec7e38c86ae2fae91ec/packages/coding-agent/src/session/speculation-lead.ts) starts within a threshold-relative band: 12.5% of the threshold, bounded to 8,192–32,000 tokens.
- Its model fallback, provider-overflow retry, branch/reset semantics and in-memory lifecycle are not adopted. Morons needs durable local-server maintenance with its existing no-replay and fixed-routing guarantees.

## Proposed first increment

### Ownership, scheduling and cost

- A concrete server-owned maintenance supervisor owns jobs, cancellation and shutdown. Application handlers only submit authenticated owner preferences/intents; the storage worker owns transactions.
- Background compaction is enabled by default with explicit cost disclosure and an owner-controlled opt-out. Preparing or discarding a summary can still consume provider quota or incur cost. Turning it off prevents new jobs and cancels/discards current work safely.
- At most one in-flight/ready job per session and one background provider request globally initially. Nonqueueing admission: if capacity is unavailable, skip that scheduling opportunity rather than accumulating work for idle sessions.
- Start only after a top-level run is terminal and the session has no active command, tool or child execution. Do not scan every historical session or depend on a connected client. An already dispatched request may finish after new foreground input arrives; that input is not queued behind maintenance.
- Keep the exact service, model, protocol revision, limits, data-use permissions and credential generation pinned to the triggering run/accepted maintenance decision. Revalidate permission and credential generation before dispatch. A UI model switch must not silently reroute an existing job.
- Reuse existing bounded compaction source/output/request-timeout policy. Give maintenance a separate provider request/continuation identity; never interleave with a live agent's opaque or sticky continuation state. No separate model selector, automatic cheaper-model choice, fallback, tools, Python memory, web requests or repository reads in the summarizer.

### Durable lifecycle and integrity

Use a separate bounded maintenance record, not a fabricated `LocalOwner` message, a child task, or a second top-level agent run. A proposed lifecycle is `Prepared → Dispatched → Ready → Installed`, with explicit failed/cancelled/uncertain/discarded terminal paths. Record dispatch before network activity; record the complete validated result before advertising readiness. Post-dispatch interruption retains possible-charge/uncertainty evidence.

A job binds at least its session, triggering run, completed source high water, source digest, parent checkpoint, context/maintenance policy, exact provider/model contract, credential generation and relevant instruction-profile binding. Persist only bounded preparation metadata and the bounded complete summary; derive source through the existing validated projector. Preserve the context-visible/`!!` distinction: hidden command text/output must never reach summarization, although canonical integrity remains bound to the complete source.

The source cut is a fixed **completed prefix**, retaining recent complete turns. Never include the current user run, incomplete call/result pairs, active child work, assistant deltas, or Python kernel memory. New input is appended after that cut, not retroactively added to a dispatched request. Automatic project guidance remains separate, pinned per run and charged to context budgets; it is not summarized or rediscovered by maintenance.

Restart recovery terminates prepared/dispatched jobs without redispatch. A committed ready result may remain usable only after full source/checkpoint/policy validation. No silent reattempt of a failed/uncertain source-prefix operation under a different job ID; a genuinely new eligible completed prefix or explicit owner action is required. Foreground compaction must consult the same attempt binding so it cannot disguise a retry of uncertain background inference.

Archive, deletion, owner disablement and server stop must drain controlled execution before lifecycle completion. Deletion removes Morons-owned maintenance data only, never the selected directory or guidance files. Persistence/integrity failures use the existing fail-closed admission/shutdown behavior; they do not invent a successful checkpoint or agent outcome.

### Readiness is not installation

A ready summary is not yet the active checkpoint. Installation occurs only at a provider-request boundary through a storage transaction that compares the expected parent checkpoint and revalidates source digest, policies, instruction compatibility and remaining-tail budgets.

- Never modify already prepared `bytes::Bytes` requests or inject a checkpoint into an active tool pair.
- Do not add background billing or latency to an unrelated run's success claim. Keep maintenance observations distinct from root and child usage.
- If a manual compaction supersedes preparation, cancel/drain and discard or explicitly consume the compatible result according to the approved manual guidance; never ignore new `/compact` guidance merely because a ready summary exists.
- Stale or insufficiently useful results are discarded explicitly. Do not automatically regenerate the same prefix.
- Preserve synchronous compaction as a bounded fallback for a new eligible operation when no useful ready result exists. A rapid context jump can still cause a wait or a durable limit failure. Never exceed token, entry, byte or image guards while waiting for speculation.

### Cache behavior and thresholds

Keep existing stable instruction/tool serialization and request ordering unchanged in this increment. Do not repeatedly replace the summary on every turn. Exact-prefix caching cannot preserve a tail's old cache merely because the tail text is unchanged after a new summary is inserted.

Evaluate a single derived pre-threshold lead band rather than adding several tuning controls; OMP's formula is a starting candidate, not a measured Morons default. Account for entry/image pressure independently of advisory token observations. Require material estimated token savings or meaningful entry/image relief, and require the resulting request to fit every independent guard. Add growth hysteresis before another automatic preparation. Select constants using deterministic pressure fixtures and measurements, not an unsupported promise of zero pauses or lower billing.

### Presentation and compatibility

Expose bounded metadata for disabled/idle/preparing/ready/discarded/failed state, timings and observed maintenance usage in `/context`; show a compact busy indicator when useful. No summary/source bodies, credentials or opaque provider continuations are sent as status metadata. Render untrusted labels through existing terminal-safe cells. Subscriber queues remain bounded and snapshots/events retain their gap-free cursor boundary.

Implementation requires a reviewed SQLite migration and typed protocol revision. Preserve legacy run/checkpoint validation and source-digest policy unless a separately justified change requires otherwise. Do not reserve final version numbers before the preceding QA repair scope is known. No dependency is selected by this design.

## Planning foundation

The first code increment separates completed-prefix cut selection and bounded source projection from the existing foreground trigger, manual guidance and per-run attempt check. Both helpers remain internal to the storage backend and are used by synchronous compaction; they do not create a maintenance job, authorize inference or install a checkpoint. The caller owns lifecycle eligibility and checkpoint validation. A projection takes a fixed session/prefix/checkpoint binding, hashes the full canonical prefix, and emits only bounded context-visible excerpts after the parent checkpoint.

Preserve current foreground selection: retain two prior complete turns when possible, then one, then at least the protected current run, with independent conservative token/item/image/byte guards at each candidate. Future idle scheduling must supply and validate its own completed window rather than manufacture an active run or reuse foreground admission as authorization. Do not add scheduling, a new threshold, preference, schema, protocol or policy revision in this extraction. The proposed runtime/cost choices above remain for the later maintenance increment.

Regression coverage for this foundation must establish fixed-prefix stability after later appends, aggregate excerpt loss disclosure, checkpoint/source separation, and unchanged whole-turn/tail protection. Existing synchronous dispatch, uncertain-effect recovery, source-integrity and manual-guidance tests remain in place.

## Durable-ledger increment

The ledger-only increment added separate maintenance jobs and append-only transition facts in SQLite 27, without a production job producer, provider dispatch, installation API, owner preference or IPC change. At that stage protocol stayed 38, tool policy 10 and source digest policy 4; the runtime increment below adds the producer and protocol observation. Runtime wiring must follow the supervision/cost contract above rather than exposing a transport shortcut to these records.

- Maintenance policy 1 binds the job/session/terminal triggering run, prepared high water, completed source cut/digest, expected parent checkpoint and instruction-profile digest. The immutable triggering run supplies the exact service/model/protocol/limits and credential generation; the binding hash also includes those values and parent summary. The instruction profile covers the current compaction/core instructions, skills/project snapshots and reviewed model data-use profile. A changed compatible-looking profile cannot silently reuse a ready result.
- At most 10,000 retained jobs globally, one prepared/dispatched/ready job per session and one dispatched job globally. Jobs are unique by session and source cut, across models and credential generations. Transition facts are bounded and never overwritten; states are Prepared, Dispatched, Ready, Failed, Cancelled, Uncertain and Discarded. Installed was reserved for the later atomic-installation increment, rather than accepted as an invented successful state in the ledger-only stage.
- Allowed transitions are Prepared to Dispatched/Failed/Cancelled, Dispatched to Ready/Uncertain, and Ready to Discarded. Cancellation after dispatch is Uncertain, preserving possible-charge evidence. Ready carries a closed, bounded complete summary/usage payload with a job-bound digest. Terminal discard retains that result and usage. No checkpoint or transcript entry is created by readiness.
- Startup validates record scope, state chains, bounds, sequences, source/binding/result digests and complete prefix boundaries before recovery. Prepared becomes Cancelled and Dispatched becomes Uncertain, never redispatched. Ready survives only when the expected parent, current instruction/model profile and credential generation remain compatible and the session is not archived. Corrupt evidence fails closed; valid but stale readiness is explicitly discarded. Integrity hashes detect corruption/rebinding, not hostile same-user database rewriting.
- Indexed source-attempt lookup is shared with foreground compaction. Automatic planning/preparation cannot repeat an already attempted maintenance or foreground prefix, even after restart or under another run/model. A new genuinely eligible cut or an explicit LocalOwner `/compact` request is required; manual guidance is never replaced by a ready result. Existing same-run idempotency does not authorize redispatch.
- Archive/delete completion rejects a still-dispatched maintenance job until its supervisor has drained it. Prepared and ready records are cancelled/discarded transactionally on archive. Deletion removes only Morons-owned records after drain and never touches the selected directory. No network or filesystem effects run from ledger recovery.

Ledger tests seed jobs through isolated storage fixtures independently of the runtime producer. They must cover transitions/rollback, restart without replay, stale versus corrupt readiness, duplicate/cross-session bindings, prefix completeness, result/usage limits, foreground no-retry/manual intent, migration and lifecycle non-interference. There is no live-background or cost qualification until supervised dispatch is implemented.

## Runtime and installation increment

The initial candidate used opt-in as a qualification safeguard. The owner requested default-on behavior, consistent with the reviewed OMP async setting. With `MORONS_BACKGROUND_COMPACTION` unset or exactly `1`, background compaction is enabled; exactly `0` disables it, and invalid/empty values also disable it rather than accidentally authorizing inference. This is not a model/tool argument or imported configuration. The server and storage worker capture the owner setting once. `/context` discloses enabled state, the last job's state, exact service/model and bounded maintenance observations separately from foreground usage. Enabled operation permits additional billable inference, including unused/discarded results. To disable, stop through authenticated IPC and restart with `MORONS_BACKGROUND_COMPACTION=0`; shutdown drains the controlled request and disabled startup discards retained readiness. No live settings toggle is introduced in this increment. Default-on does not authorize startup/history scans, speculative tools, model substitution or uncertain retries.

A separate supervisor has one nonqueueing global slot and one owned cancellation/task handle. It schedules only after a successful top-level run has stopped, not after cancellation/failure, not on startup and not from a historical-session scan. The storage service rechecks the latest terminal triggering run, idle session/commands, pending archive/delete intent, exact credential generation, model/profile, prefix attempts and 10,000-job cap before preparation; it repeats idle/binding checks before durable dispatch. An undispatched preparation overtaken by new input is cancelled; a dispatched request may finish without holding foreground admission. SQLite work still serializes on the bounded worker. Existing credential-dispatch leases remain unchanged and can delay foreground inference while another request establishes its response headers; this increment guarantees nonqueued input acceptance, not zero foreground waits or altered credential-rotation semantics.

Use the derived lead `70% of model input capacity minus (one eighth of that threshold, clamped to 8,192–32,000)` plus independent pre-pressure at 160 reserved items or five-eighths of image count/bytes. The same lead also applies before the conservative hard input limit: a low advisory provider estimate cannot suppress preparation when conservative usage reaches `maximum input minus lead`. This does not lower or relax any independent hard guard. These conservative first constants are deterministic policy, not measured billing/latency improvements. Require projected material relief against the maximum summary reserve before dispatch, and recheck relief against the actual summary before installation: at least 4,096 conservative tokens or 24 context entries or image relief. A growing eligible source cut plus this minimum relief supplies hysteresis. Background cut selection also targets a conservative retained context at or below `soft threshold minus lead`, using the maximum summary reserve, and below independent item/image pressure. It tries retaining two prior whole turns, then one, then the protected triggering turn; if that protected turn cannot meet the target, the opportunity is skipped. Installation rechecks the same headroom with the actual summary and receiving tail. A ready result that would immediately trigger foreground compaction is discarded rather than installed and summarized again. This deliberate headroom target trades some preparation opportunities and older active detail for fewer back-to-back summaries; canonical history stays intact. Summarization is one tools-free request with a distinct job conversation identity and a 180-second whole-job deadline, never fallback or replay.

Lifecycle cancellation is serialized through drain and final storage cleanup, not merely removal of the active task handle. Concurrent cancellation/stop cannot mark a dispatched request terminal before its owning drain completes, and replacement admission cannot race with session-scoped cleanup. Admission skips an occupied lifecycle lock instead of queueing behind a drain. The task remains registered while being joined, so cancellation of a drain caller cannot detach controlled execution.

Installed is state 8 with an append-only event referencing the checkpoint, receiving run and request high water. Installation is a storage transaction immediately before the receiving run's first provider request only: it must be Active, uncancelled, have no provider preparation or open tool call, and not be a manual `/compact`. Revalidate complete source/binding/result, expected current parent, exact receiving model/protocol/limits/credential/profile and all conservative tail guards. A valid stale/unhelpful result is discarded, not silently regenerated. Insert checkpoint and Installed event atomically; accepted-input accounting stays bound to its original checkpoint. Results arriving after the first provider turn wait for a later run. Manual compaction drains/discards maintenance and preserves new user guidance. No already prepared bytes or live continuation is changed.

The unreleased SQLite 27 migration is extended with Installed references and indexes; protocol 39 adds bounded observation DTOs, independent of storage records. No previously published schema 27 store exists in this branch. Retained QA stores must not be upgraded casually. Normal ledger validation/recovery, exact routing, disabled startup and authenticated lifecycle controls remain fail closed. Runtime tests use loopback providers and explicit test opt-in; credentialed behavior/performance qualification remains separate.

## Tool-result accounting correction

Runtime context budgeting selects payload bytes by canonical entry kind: tool-call entries charge arguments and tool-result entries charge results. Both rows join the call, so a `COALESCE(text, arguments, result)` projection incorrectly charges arguments twice and can miss compaction pressure before materialization. The corrected budget applies to observations, prefix selection and foreground/background hard guards without increasing limits or changing source digests. Persisted acceptance estimates retain their historical encoding for exact validation; they are bookkeeping, not dispatch permission, and are not rewritten. Actual provider requests still require independently recomputed budgets. A live-shaped loopback regression must demonstrate that large tool results trigger preparation before another request exceeds capacity.

## Implementation sequence and acceptance tests

1. Resolve/bound the live QA findings with deterministic reproductions and redacted diagnostics; do not loosen validation or increase child budgets blindly.
2. Extract only the reusable completed-prefix planning/projection logic from `persistence/backend/context_compaction.rs`. Current active-run checks in `backend/compaction.rs` remain intact; add maintenance operations beside them instead of bypassing them with a fake active run.
3. Add bounded records and transition/recovery tests: interrupted preparation/dispatch, committed readiness, stale parent/source, credential or model change, duplicate scheduling and no uncertain replay.
4. Add supervised idle-triggered execution with loopback providers. Test new input during preparation, one-slot admission, disable/archive/delete/stop while blocked, cancellation drain, worker failure and session isolation.
5. Add atomic request-boundary installation and tests for immutable prepared bytes, complete tool pairs, current-run protection, manual guidance, project-profile changes, `!!` exclusion, image/item pressure and insufficient savings.
6. Add bounded UI/observations, migrations and platform CI. Measure preparation cost, foreground wait time, reuse/discard rate, conservative context relief and maintenance usage separately.
7. Perform fresh credentialed behavior QA before enabling the option for routine use. Starting new speculation during active runs, broader concurrency and provider-native compaction are separate later increments.

## Non-goals

No OAuth implementation, imported Pi/OMP/Codex credentials, new model/protocol fallback, speculative tool execution, automatic replay, hidden cross-session memory, filesystem snapshot/rollback, provider cache guarantee or claim to reproduce ChatGPT's internal architecture.
