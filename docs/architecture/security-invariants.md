# Security invariants

ADR 0012 defines Morons as a trusted-local coding-agent harness. These invariants describe the guarantees Morons does and does not provide after that reset.

## Trust posture

- Morons is not a security sandbox. Model-selected filesystem operations, shell commands, IPython cells, skill resources, and their descendants execute with the local user's operating-system authority.
- Repository content, model output, skills, web content, images, command output, provider data, and protocol input remain untrusted data even though the user deliberately grants tools local authority.
- No prompt, tool description, path check, model annotation, command prefix, or attachment marker is a security boundary.
- Morons provides no rollback, containment, filesystem isolation, network isolation, credential isolation from same-user processes, or guarantee that cancellation reverses an effect that already occurred.
- Users requiring containment must run the complete Morons application in an externally managed container, virtual machine, or restricted operating-system account.
- The application must disclose this posture during onboarding and in help. It must not describe lifecycle supervision, process separation, or bounded output as sandboxing.
- Approval prompts are not required by default and must not be represented as a security boundary if later introduced.

## Direct working directories and sessions

- Every session binds one durable absolute selected working-directory locator. The locator is required for resume and is never authorization evidence.
- A session never imports, copies, snapshots, generations, publishes, exports, resets, cleans, checks out, or deletes the selected working directory as part of session lifecycle.
- Relative tool paths and command working directories resolve from the selected directory. Absolute paths and normal operating-system path semantics are intentionally available.
- Path validation prevents malformed protocol values and implementation mistakes; it must not imply confinement that `bash` or `ipython` can bypass.
- Before starting work, Morons verifies that the selected locator currently resolves to a directory. It does not silently retarget a session when the directory is missing or moved.
- Multiple sessions may reference the same directory. Their transcripts and contexts are independent, but their filesystem effects may race.
- One session has at most one nonterminal top-level run. Different sessions may run concurrently only within bounded global capacity.
- Session switching and client detachment do not cancel work or transfer session authority.
- Deleting a session may remove only Morons-owned records, attachment files, and temporary runtime state. It must never remove or modify the selected working directory.

## Local tools and process lifecycle

- The MVP model tool catalog contains only `read`, `write`, `edit`, `bash`, `web_search`, `ipython`, and `task`.
- Tool names, inputs, results, counts, text, paths, collection sizes, and encoded payloads are independently bounded and strictly decoded.
- `read` returns bounded text or a normalized bounded image. `write` and `edit` apply directly to the requested filesystem target and report only the result Morons can establish.
- `edit` requires exact, unique, nonoverlapping replacements and fails rather than guessing at an ambiguous mutation.
- `bash` uses a compatible Bash interpreter, starts in the selected directory, closes standard input, and captures standard output and standard error without a PTY.
- Shell and IPython execution intentionally receive the user's normal development authority, including ordinary filesystem access, network access, `PATH`, Git configuration, credential helpers, signing agents, SSH agents, and environment credentials.
- Morons must not inject its stored OpenCode credential, local IPC authentication key, or internal provider authorization headers into child arguments or environments.
- Credentials independently available to the local user may be read or emitted by a child process. Once emitted through a context-bearing command or tool, that data may be persisted and sent to the model. Morons must document this residual risk rather than claim to prevent it.
- Child environment values are bounded transient execution input. Morons must not intentionally persist, audit, log, render, or send the environment to a provider unless a command itself emits a value.
- Shell commands, command mode, IPython cells, web requests, and descendants have explicit wall-clock, inactivity, output, process-count, and aggregate-run limits.
- Cancellation, timeout, output exhaustion, kernel restart, graceful shutdown, and client-requested stop terminate the complete process tree owned by the operation. Morons must not report termination until controlled descendants are known to have stopped or the result is explicitly uncertain.
- Process-tree control and bounded pipes are lifecycle controls, not containment. Effects completed before termination remain applied.
- Standard input remains closed for model-selected and command-mode subprocesses. Morons exposes no user-facing subprocess PTY, interactive terminal, terminal emulator, or SSH surface.
- The persistent IPython kernel is one temporary runtime per active session. By default it uses the versioned Morons-owned Python and hash-locked Jupyter dependencies prepared by the exact checksummed packaged `morons-uv`; `MORONS_PYTHON` is an explicit expert override. At most four kernels remain live; least-recently-used idle kernels are evicted. Kernel memory is never authoritative and may be lost on cancellation, limit exhaustion, eviction, failure, or restart.
- Managed Python preparation is locked, staged, atomically published, cancellable, time-bounded, isolated from repository and user Python configuration, and separate from credentials, SQLite, attachments, system Python, and user virtual environments. A partial or invalid stage is never executable as the active runtime.
- Morons injects no helper API, managed provider credential, or generic privileged server proxy into IPython kernels. Model-facing Morons tools remain separate bounded typed tool calls.

## Subagents

- `task` accepts one explicit bounded shared context and one to three bounded self-contained assignments. It is the only subagent admission surface.
- Children use the parent's exact reviewed service and model by default. A server-authoritative owner setting may instead pin one exact reviewed child service/model/protocol pair per task batch; children still use the parent's accepted credential generation and selected directory. Prompts, repositories, skills, and children cannot select an endpoint, credential, directory, unreviewed model, fallback, or additional capability.
- A child receives only fixed server instructions, the selected directory, shared context, and its assignment. Parent history, compaction checkpoints, images, reasoning continuation, active skill bodies, IPython memory, and sibling context are not inherited implicitly.
- Children receive only `read`, `write`, `edit`, `bash`, and `web_search`. They receive neither `task` nor `ipython`, so recursion depth is one and temporary kernel state is not shared.
- Up to three siblings in one call execute concurrently under a global four-child limit. They share the real selected directory and may race; assignment guidance is not isolation or a security boundary.
- The parent tool loop blocks until every child terminates. There is no background child registry, messaging channel, idle revival, hidden result injection, or independently resumable child session.
- Each child has independent provider-turn, tool-call, mutation, context, output, wall-clock, and usage bounds. Results return in assignment order and include bounded reports and usage.
- Each child has a stable opaque OpenCode conversation identifier distinct from its parent and siblings. It is derived from parent session and canonical task-call identity, never exposed, and follows all existing inference-header redaction rules.
- The canonical parent `task` call and terminal result are the durable effect boundary. Nested activity is never replayed. A crash after dispatch makes the outer operation uncertain and recovery performs no child provider, process, web, or filesystem effect.
- Parent cancellation and task timeout fan out to every running or waiting child, and completion is not reported until controlled children and descendants stop. Completed effects are not rolled back.

## Command mode

- A prompt beginning with `!` is command mode rather than model input.
- `!command` executes locally, displays bounded output, and deliberately makes the bounded command and result eligible for later provider context.
- `!!command` executes locally and displays bounded output, but neither the command nor its output may enter provider context or a compaction summary.
- Completed and interrupted command-mode operations are durable local-command entries with an explicit context-visibility field; they are not attributed as user or assistant messages.
- `!!` is a context-control feature only. It is not a secrecy guarantee, and its command and bounded output may persist in owner-controlled local session history.
- Command mode is rejected while the same session has an active model run.
- Both modes use the same selected directory, shell, environment, cancellation, deadline, output, terminal-safety, and process-tree rules as `bash`.

## Skills

- Morons supports Markdown `SKILL.md` directories conforming to the Agent Skills format; it does not treat a skill as a trusted binary extension or new capability.
- Skill discovery is bounded to documented bundled, user, and project roots, rejects linked skill directories and files, requires standard names to match their parent directories, and applies deterministic precedence and collision handling.
- Skill names and descriptions are validated and bounded before entering the system prompt. Full instructions and referenced resources are loaded only when selected.
- A standalone whitespace-delimited exact installed `@name` token invokes that skill and binds its complete bounded instructions to the accepted run. Unknown `@` tokens remain prompt text.
- `@` is reserved for skills and must not be interpreted as a file-path authority.
- Skills, their scripts, references, and assets are untrusted local content and may direct the model to use every authority already available to tools.
- The bundled skill creator writes only through ordinary tools and does not bypass filesystem or execution semantics.

## Images and attachments

- Clipboard, drag-and-drop, explicit path, and `read` image ingestion converge on one provider-neutral structured attachment representation.
- Attachment bytes are captured immediately; temporary clipboard or dropped paths must not be required for later submission or resume.
- File type is determined from validated content rather than trusted solely from an extension or clipboard label.
- Decoding, orientation, conversion, resizing, dimensions, attachment count, individual bytes, aggregate bytes, encoded provider payload, and processing time are independently bounded.
- Supported normalized provider formats are PNG, JPEG, WebP, and GIF. Other admitted decodable inputs are converted to PNG or rejected clearly.
- The editor marker, such as `[puppies.png]`, is atomic presentation and prompt-order metadata. It is not the image payload, a trusted path, or authorization evidence.
- Attachment files reside only in Morons-owned per-session state and are referenced by durable metadata. Session deletion never follows attachment metadata outside that state.
- A selected model must have a reviewed image-input capability before dispatch. Unsupported submission retains the draft and fails clearly; images are never silently discarded.
- Provider adapters receive only normalized bounded bytes, media type, ordering, and required metadata. Local source paths are not needed in provider payloads.
- Image metadata, decoder errors, filenames, and model-produced visual descriptions remain untrusted terminal and context input.

## Context management

- The complete bounded canonical transcript remains durable. Context compaction never deletes, mutates, or replaces canonical messages, attachment references, tool calls, tool results, or local-command entries.
- Provider context is reconstructed through a deterministic versioned server-owned policy and never depends on provider-hosted conversation retention.
- Every root provider dispatch binds an exact canonical source-entry high water, selected model, context-policy version, limits, and active skill set. Each task-child dispatch instead binds the committed parent run selection, canonical outer task-call identity, child index, explicit shared context, assignment, and child limits.
- Provider response identifiers and opaque continuation data are transient run state. Only continuation required by one live tool loop may remain in bounded trusted memory.
- A compaction checkpoint covers an exact ordered source prefix, records its digest and high water, and contains a bounded model-generated summary. Invalid coverage or lineage is never used.
- Context assembly retains current developer instructions, tool contracts, project instructions, active skill instructions, the newest valid summary, and an uncompacted recent tail in canonical order.
- The policy reserves model-specific capacity for output, tools, and bounded tool-loop growth before dispatch.
- If an old completed prefix no longer fits, Morons compacts it before a later dispatch. If the current run alone cannot fit, the run fails clearly rather than silently dropping current information.
- Old image bytes covered by compaction remain durable session attachments but are not resent automatically. Their relevant observations belong in the summary.
- Context summaries are lossy untrusted model output. They never authorize an operation, establish filesystem state, override developer instructions, or become canonical history.
- `!!` commands, transient child environments, terminal-only state, provider errors, partial assistant text, and IPython memory are excluded from summaries.
- No hidden memory crosses sessions. Embeddings, vector storage, and automatic project memory are outside the MVP. A `task` child receives only its explicit ephemeral parent-scoped handoff and returns only a bounded canonical report.

## Local IPC trust boundary

- Protocol-version negotiation is compatibility checking, not authentication or authorization.
- Local transport authentication completes before either process exchanges application protocol messages.
- The server authorizes the operating-system peer before reading application bytes or disclosing an authentication challenge.
- The client authenticates the connected server before sending application messages, credentials, selected paths, prompts, attachments, or transient execution values.
- Authentication failures close the connection without an application protocol response.
- Automatic server startup is allowed only after secure control-state classification; malformed, insecure, authentication-failed, or protocol-mismatched state fails closed without replacement.
- A client-started server is the exact packaged companion executable launched without a shell or untrusted executable-path selection.
- A process identifier, exit status, readiness string, or successful spawn is never server-authentication evidence.
- Each control root retains one persistent random 256-bit authentication key and one stable owner-only host lock. Every server process receives a fresh random 128-bit Host Epoch and endpoint.
- HMAC-SHA256 proofs use fresh client and server nonces, distinct role tags, the authentication protocol version, and the Host Epoch, and are verified with a constant-time API.
- The authentication key never crosses IPC or appears in registrations, endpoint names, logs, audit events, prompts, arguments, or child environments.
- Same-user processes are inside the operating-system-user trust boundary. Without external containment they may be able to read owner-controlled control state and authenticate as the local owner.

## Application service boundary

- Transport authentication admits a peer but does not by itself validate an application operation or resource scope.
- Every transport adapter invokes the same server-owned validation, session scope, limits, idempotency, and audit behavior.
- Resource identifiers are opaque locators and are never authorization evidence.
- Retriable mutations use stable request identity, and uncertain external side effects are never retried blindly.
- Protocol responses and events are deliberate sanitized DTOs rather than persistence records, provider payloads, logs, environment snapshots, or raw process streams.
- Event subscriptions are scoped to a session, resumable through server-validated durable cursors, and composed with snapshots without losing committed events.
- Ephemeral assistant deltas identify an exact session and run, remain bounded and ordered, are never replayed, and are replaced by one complete committed assistant message.
- Subscriber queues are bounded; slow consumers are disconnected rather than allowed unbounded memory growth.
- Authenticated local IPC remains the only application transport. A network listener requires another architecture decision and threat-model update.

## Provider credentials and model egress

- Only trusted server provider code reads Morons-managed OpenCode credentials or attaches provider authorization headers.
- Persistent provider credentials remain in dedicated owner-controlled state separate from SQLite, backups, configuration, attachments, and local IPC control state.
- Provider credentials never intentionally appear in command arguments, child environments, SQLite, backups, request fingerprints, audit facts, prompts, attachment metadata, protocol responses, errors, or logs.
- This logical custody does not provide confidentiality from arbitrary same-user processes in the trusted-local model. Documentation must not imply otherwise.
- Credential input crosses IPC only after mutual authentication, is collected without echo, uses redacted secret-bearing types, and is never automatically retried after an unknown mutation outcome.
- Credential status returns no key bytes, prefix, suffix, verifier, or credential-derived fingerprint.
- Every run records the accepted credential generation, and a dispatch under a stale generation fails before network transmission.
- Production provider and web-search requests use fixed reviewed HTTPS origins and paths, disabled redirects, normal certificate and hostname verification, and exact authorization-header scoping.
- Every OpenCode Zen and Go inference request carries one stable `x-opencode-session` value derived from a Morons-owned conversation identity. A root value remains stable across runs, compaction, and tool turns in its durable session. Each task child receives a distinct value stable across that child's turns and derived from the parent session, canonical task-call identity, and child index. Values differ across unrelated conversations, are not raw local locators, and are never attached to public catalog requests or emitted in logs, errors, persistence, or protocol responses.
- Remote catalogs may narrow but never enlarge the reviewed model, protocol, capability, limit, route, or data-use manifest.
- The durable global default model is bounded convenience state selected only from the reviewed manifest. It never authorizes a provider, endpoint, credential, capability, or run; every submitted run still carries and receives authoritative validation of its exact service and model.
- Provider requests, responses, streams, errors, usage, identifiers, tool arguments, attachments, and output are bounded, strictly decoded, and sanitized at application boundaries.
- An inference request is never retried automatically after dispatch because it may already have incurred billing or another external effect.
- Repository content, prompts, command output, images, skill content, and tool results leave the local machine only when included by the deterministic context policy for a deliberate provider dispatch.
- Web search sends only the bounded query to Brave Search's fixed API endpoint. Its `BRAVE_SEARCH_API_KEY` is ordinary process-environment state rather than a Morons-managed credential: Morons attaches it only to that endpoint and does not persist, audit, log, render, or include it in provider context. Like every ordinary environment credential, it is intentionally available to local child execution and therefore has no confidentiality guarantee from same-user tools.

## Durable state and recovery

- SQLite is authoritative for durable session metadata, transcript order, run state, idempotency, subscription cursors, context checkpoints, and attachment references.
- Morons-owned attachment files are authoritative only through validated database references and remain confined to owner-controlled per-session attachment state.
- Only the lifetime host-lock owner opens the database, through one bounded server-owned storage worker.
- Durable payloads are bounded, strictly decoded, explicitly versioned, and independent of Rust layouts, native word size, protocol DTOs, and provider wire objects.
- Complete attributed user messages commit with one run identity before execution begins. Complete assistant messages, bounded tool results, and bounded local-command results commit before later provider use.
- Partial assistant text, child environments, kernel memory, live process state, and temporary provider continuation are never authoritative durable state.
- Direct filesystem and external command effects are not atomic with SQLite. A crash may leave an applied effect without a committed result.
- Startup never reruns a provider call, tool call, command, Python cell, web request, or uncertain external effect. It terminates nonterminal runs as interrupted or uncertain using only committed facts.
- Recovery does not inspect, reset, delete, or repair the selected working directory automatically.
- Storage quotas reject new work before uncontrolled growth and never silently delete canonical history to regain space.
- Schema migrations remain ordered and transactional; newer, corrupt, foreign, or unsupported state fails closed without automatic recreation or downgrade.
- Live database backup uses SQLite's online backup API. A database-only backup does not include selected project files or file-backed image payloads and must not be described as a project or complete session backup.

## Terminal and local presentation

- User text, provider text, skill metadata, paths, filenames, errors, web content, command output, IPython output, and attachment metadata are untrusted terminal input.
- Untrusted text is converted to bounded terminal cells without forwarding control sequences, operating-system commands, hyperlinks, title changes, device controls, clipboard operations, or bidirectional formatting controls.
- Untrusted output is never written through raw ANSI paths. Trusted terminal control is emitted only by the reviewed Ratatui backend.
- Terminal mode and screen ownership are restored on ordinary exits and handled failures without printing credential, environment, prompt, attachment, or transcript buffers.
- Clipboard and drag-and-drop events are bounded before decoding and cannot directly become terminal control output.
- The Ratatui client presents command output but is not a PTY, terminal emulator, shell process host, or editor.

## Processor and platform portability

- Release targets remain the six `x86_64` and `aarch64` macOS, Linux, and Windows targets selected in ADR 0007.
- Protocol, authentication, persistence, attachment metadata, fingerprints, and cursors use explicit widths and byte order and never serialize pointers, `usize`, native layouts, handles, or host-endian values.
- Conversions among protocol lengths, SQLite integers, file sizes, image dimensions, token estimates, and `usize` reject overflow and truncation.
- Processor architecture is never authentication, authorization, or capability evidence.
- Native validation remains required before claiming release support because cross-compilation cannot prove IPC peer identity, filesystem controls, clipboard behavior, terminal restoration, shell discovery, process-tree termination, kernel lifecycle, or SQLite durability.
- Distribution artifacts pair client and server executables from the same revision and target and never download an executable selected by repository, model, or registration input.
