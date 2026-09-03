# Threat model

ADR 0012 changes Morons from a sandboxed repository-copy system into a trusted-local coding-agent harness. This threat model is explicit about the resulting authority and residual risk.

## Protected assets

- Morons-managed OpenCode credentials and billable provider usage
- Local IPC authentication key, host lock, and endpoint registration
- Durable session transcripts, selected-directory metadata, attachments, context checkpoints, and run state
- Authoritative SQLite data, migration backups, and durable event history
- Terminal presentation integrity and non-echoing credential input
- Packaged client and companion-server executable identity
- User project files, home-directory data, environment credentials, Git credentials, signing agents, SSH agents, cloud credentials, and other resources available to the local account

The final category is user-owned authority that Morons deliberately grants to local tools. Morons does not claim to protect it from model-selected commands.

## Untrusted inputs

- IPC clients and application protocol messages
- Session, run, message, mutation, model, cursor, path, command, cell, query, and attachment inputs
- Endpoint, registration, database, backup, attachment, and selected-directory filesystem state
- Repository files, names, links, metadata, configuration, dependencies, hooks, and concurrent changes
- Model output, reasoning, tool names, arguments, paths, generated commands, subagent shared context, assignments, and reports
- Skills, their Markdown instructions, scripts, references, and assets
- Web search results and fetched external content
- Clipboard data, drag-and-drop paths, image bytes, metadata, filenames, and decoder behavior
- Shell, Git, compiler, test, package-manager, child-process, and IPython output
- Terminal key, paste, resize, mouse, and rendering input
- Provider model catalogs, HTTP headers, error bodies, SSE records, usage, identifiers, and content

## Trust assumptions

- The local user deliberately authorizes Morons to run model-selected tools with that user's operating-system authority.
- The operating system correctly enforces user identity, filesystem permissions, process creation, process-tree termination, local IPC controls, and owner-only Morons state.
- Root, LocalSystem, administrators, and equivalent privileged identities are outside the local guarantee.
- Malicious processes already running as the same operating-system user are outside the local IPC and credential-confidentiality guarantee.
- The selected Bash installation, IPython installation, language runtimes, Git, credential helpers, agents, dependencies, and ordinary user environment are controlled by or accepted by the user; Morons does not attest to them.
- OpenCode and its upstream providers receive context deliberately selected for an authorized run. Their infrastructure, policy, catalogs, responses, and model output remain external and untrusted.
- Public certificate authorities and the operating system's TLS implementation correctly authenticate fixed provider HTTPS origins.
- Users needing containment run the complete Morons application inside an external boundary that they configure and validate.

## Explicitly accepted local-execution risks

A model-selected command, Python cell, skill script, dependency, hook, compiler, test, package manager, or descendant can, with the user's authority:

- read, alter, delete, encrypt, or corrupt project and non-project files;
- inspect Morons-owned owner-readable control, credential, transcript, or attachment files;
- discover the local IPC endpoint and authenticate as the same local owner if it obtains owner-readable authentication state;
- read environment variables, shell configuration, history, SSH agents, signing agents, Git credential helpers, browser-accessible state, cloud configuration, and keychains available to the account;
- send source code, credentials, personal data, or other files over the network;
- push commits, create releases, mutate remote repositories, publish packages, spend cloud resources, or perform other external side effects;
- install or execute persistent software outside Morons' process tree;
- race another Morons session or ordinary user process in the same directory; and
- leave completed filesystem or external effects after cancellation, timeout, client exit, or server failure.

These are not sandbox escapes because the MVP establishes no sandbox. Lifecycle limits reduce hangs and uncontrolled output; they do not constrain authority. Onboarding and help must state this clearly.

## Local IPC threats

- A different local user connects to the legitimate server.
- A process impersonates the server or occupies a predictable endpoint before startup.
- A process replays stale registration, endpoint, Host Epoch, process ID, nonce, or proof data.
- Concurrent startups race to publish endpoints or delete successor state.
- A process tampers with the authentication key, host lock, control directory, or registration.
- A link or reparse-point race redirects control-state validation or cleanup.
- A fake server obtains credentials, prompts, attachments, paths, or execution environment data before authentication.
- A client sends malformed, oversized, partial, replayed, or stalled records.
- A fake companion executable is selected through `PATH`, repository content, configuration, or a writable installation-relative path.
- A process identifier, exit status, or readiness output is mistaken for server identity.

Same-user commands obtaining owner-readable IPC state are an accepted residual risk under the trusted-local posture, not an assurance provided by HMAC authentication.

## Application and session threats

- A transport-authenticated client accesses another session or invokes an operation outside its validated scope.
- A client replay duplicates session creation, input, cancellation, deletion, credential mutation, or another external effect.
- A forged or cross-session resource identifier or cursor exposes state or loses committed events.
- Snapshot/subscription races omit events, and unbounded subscribers exhaust memory.
- A client forges assistant messages, tool calls, tool results, run transitions, or terminal outcomes.
- Concurrent input creates more than one top-level run in a session or bypasses global capacity.
- A task batch exceeds child, depth, provider-turn, tool-call, mutation, context, output, time, or global concurrency limits.
- A child implicitly receives parent history, skills, images, kernel memory, sibling state, or another session's context.
- A child recursively delegates, selects another provider or model, or continues after the parent task has reported completion.
- Concurrent children race on the shared selected directory and overwrite or invalidate one another's observations.
- A child result is injected out of assignment order, omitted, duplicated, or treated as durable before the outer task result commits.
- Switching or closing a client implicitly cancels background work.
- Session deletion follows a selected-directory or attachment path and deletes user files.
- A missing or moved working directory is silently retargeted.
- Two sessions sharing a directory race and each model acts on stale observations.
- An oversized transcript, attachment collection, tool result, command output, or event stream exhausts storage or memory.

## Direct filesystem and process threats

- A malformed path, encoding, integer conversion, or platform path form reaches an unintended target because of an implementation bug.
- A model mistakes a path restriction for confinement and then reaches the same target through Bash or Python.
- An exact edit applies to ambiguous or stale content.
- A write, command, Python cell, Git operation, or dependency process partially changes files before failure.
- A process forks, daemonizes, retains handles, or survives cancellation and continues changing files.
- Standard output or error deadlocks a child, grows without bound, contains invalid bytes, or injects terminal control sequences.
- A command waits for standard input or a PTY that Morons does not provide.
- Bash selection differs from the shell syntax described to the model, especially on Windows.
- A transient user environment is logged, persisted, sent to a provider, or confused with Morons-managed provider credentials.
- A command prints a user credential that then enters transcript history or provider context.
- A server crash leaves a local or remote side effect without a committed tool result.
- Recovery repeats a command, provider request, Python cell, web request, Git push, or another uncertain effect.

## Provider and credential threats

- A Morons-managed credential is exposed through arguments, child environments, debug output, logs, errors, audit facts, SQLite, backups, prompts, attachments, or protocol responses.
- A same-user process reads owner-controlled provider credential files directly. This remains a residual risk without operating-system containment.
- A fake client submits a credential before authenticating the server, or an operation returns credential-derived material.
- A missing, malformed, linked, stale, partially replaced, or insecure credential file is accepted as valid state.
- A credential mutation races, is blindly retried after an unknown outcome, or loses its recovery boundary.
- A run dispatches with a stale credential generation.
- Repository, model, configuration, catalog, proxy, certificate override, or redirect input selects an attacker-controlled inference origin or credential scope.
- A malicious provider sends malformed, oversized, endless, contradictory, or terminally inconsistent streams.
- A remote catalog adds an unreviewed model, capability, protocol, limit, route, or data-use policy.
- A dispatched inference request is retried and incurs duplicate usage.
- Provider response identifiers become authoritative conversation state and make local recovery depend on external retention.
- A context-bearing command, tool, skill, or image unintentionally sends sensitive local content to a provider.
- A malformed web query, redirect, proxy setting, or response causes the Brave Search credential to be sent outside its fixed reviewed endpoint, or the credential is persisted, logged, audited, rendered, or included in model context. The environment-supplied credential remains deliberately visible to same-user child execution.
- A missing or rotating `x-opencode-session` value defeats OpenCode routing and prompt-cache affinity, while reusing one value across unrelated root or child conversations creates unintended correlation and traffic concentration.
- Concurrent child inference multiplies provider usage, exceeds expected spend, or lets credential replacement race a later child turn.

## Skills and prompt threats

- A discovered skill shadows another skill nondeterministically or uses invalid metadata to enter the prompt.
- An `@` mention in an email address, package name, or username is mistakenly invoked as a skill.
- A skill claims that its instructions or `allowed-tools` metadata grant authority not present in the fixed tool catalog.
- A malicious project skill directs the model to exfiltrate data, modify unrelated files, or execute destructive scripts.
- Recursive skill discovery, references, or assets consume unbounded time, memory, or context.
- Skill instructions, repository files, web pages, or images attempt prompt injection or impersonate developer instructions.
- A skill-creator operation overwrites an existing skill or writes outside the user's selected skill root unexpectedly.

## Image and clipboard threats

- An image decoder receives malformed, adversarial, decompression-bomb, oversized-dimension, animated, or unsupported data.
- Clipboard access blocks indefinitely, returns mislabeled content, or invokes an unsafe platform fallback.
- Drag-and-drop path parsing treats ordinary pasted text as a file path or mishandles spaces, quoting, `file://`, Windows paths, or shell escapes.
- A temporary pasteboard path disappears after marker insertion but before bytes are captured.
- An attachment filename injects terminal controls, path traversal, prompt syntax, or misleading Unicode.
- A marker is submitted without its image or is reordered relative to prompt text.
- A model without image input silently receives text without the attached visual evidence.
- Base64 expansion or repeated session serialization causes request, memory, database, or backup growth.
- Session deletion follows a forged attachment reference outside Morons-owned attachment state.
- An old image is omitted during compaction without preserving relevant findings in the summary.

## Context and persistence threats

- A malformed or newer schema, corrupt canonical entry, attachment reference, or compaction checkpoint is accepted.
- Partial assistant text or temporary provider continuation becomes durable authoritative history.
- Context construction omits, duplicates, reorders, or crosses session entries.
- A summary covers the wrong source prefix or is accepted without binding its exact high water and digest.
- Compaction deletes canonical history or makes a lossy summary the only record.
- The system compacts the current active turn, silently drops a recent image, command, or tool result, or exceeds the model limit despite preflight.
- A model-generated summary fabricates filesystem state, loses a user constraint, repeats prompt injection, or is treated as authorization.
- `!!` command content enters provider context or a compaction summary.
- Hidden memory or a compaction summary leaks from one session into another.
- Provider-hosted continuation is required after restart and prevents deterministic local reconstruction.
- A crash commits projections or events without canonical entries, publishes success before commit, or repeats an external effect whose outcome is unknown.
- Unbounded history, checkpoints, attachments, idempotency records, or event backlogs exhaust disk.
- A live SQLite file is copied inconsistently or a database-only backup is mistaken for a project backup.

## Terminal threats

- User, provider, skill, path, filename, error, web, command, or Python text injects escape sequences, hyperlinks, terminal-title changes, clipboard operations, device commands, or bidirectional layout controls.
- Large paste, clipboard image, resize storms, or delta streams exhaust client memory or block input.
- Credential entry is echoed, copied, stored in input history, rendered after cancellation, or retained across connection loss.
- Terminal restoration prints sensitive buffers or leaves the terminal in raw/alternate-screen mode.
- Filename attachment markers become editable text and lose their structured payload association.
- The command output view is mistaken for an interactive shell or receives unintended input.

## Mitigations

- Disclose the trusted-local authority model prominently and require external containment for users who need isolation.
- Keep the tool catalog fixed and small while applying strict schema, count, byte, time, and output bounds.
- Admit subagents only through the closed batched `task` schema; cap a batch at three children, global execution at four children, recursion at one level, and each child independently by provider turns, tool calls, mutations, context, output, and time.
- Give children only fixed instructions, selected-directory metadata, explicit shared context, and one assignment; do not inherit parent transcript, checkpoints, images, active skill bodies, reasoning continuation, IPython memory, or sibling context.
- Block the parent until all children terminate, preserve input-order results, and provide no background registry, revival, messaging, or hidden result-injection path.
- Treat the outer canonical task operation as the durable no-replay boundary and fan parent cancellation out through child provider, web, shell, and process-tree execution.
- Use exact unique replacements for `edit` and report direct filesystem errors without claiming rollback.
- Run commands without a PTY or standard input, drain bounded streams concurrently, and terminate complete owned process trees on cancellation or limits.
- Treat process supervision as lifecycle control only and never describe it as confinement.
- Supply the intended bounded user execution environment only to local execution paths; never inject Morons-managed provider credentials.
- Persist complete bounded transcript entries and attachment references, keep deltas and runtime memory ephemeral, and never replay uncertain external effects.
- Use deterministic versioned context construction, source-bound compaction checkpoints, recent-tail retention, model-specific reserves, and clear context-limit failures.
- Exclude `!!` commands and transient environments from provider context and compaction.
- Validate bounded Agent Skills YAML metadata and matching parent-directory names, reject linked skill entries, apply deterministic precedence with fail-closed collisions, progressively load instructions, and invoke only standalone exact installed `@name` tokens whose complete instructions are snapshotted with the accepted run.
- Capture clipboard and dropped image bytes immediately, detect type from content, normalize under dimension and encoded-size limits, store bytes once, and fail clearly for non-vision models.
- Authorize operating-system peers before application exchange and require randomized endpoints, owner-only control state, a lifetime host lock, and role-separated HMAC proofs.
- Start only the exact packaged companion without a shell or untrusted executable-path selection.
- Keep provider and web-search routes fixed in reviewed code, disable redirects, scope authorization headers exactly, strictly decode bounded remote responses, and never retry dispatched inference or web search automatically.
- Derive one opaque `x-opencode-session` value per Morons conversation: preserve a root value across its durable session, derive a distinct stable value for each canonical task child, rotate values across unrelated conversations, omit them from catalog requests, and never log or persist a derived header.
- Store Morons-managed credentials outside SQLite and never intentionally include them in child environments, prompts, provider payload bodies, errors, logs, or audit facts.
- Use one bounded storage worker, transactional canonical-entry and projection commits, ordered migrations, online SQLite backup, quotas, and startup recovery that performs no external effect.
- Scope subscriptions and cursors to sessions, compose snapshots and replay at one high water, and disconnect slow consumers.
- Render all untrusted content through bounded terminal-safe Ratatui cells and restore terminal ownership on exit.
- Keep protocol and persistence encodings architecture-neutral and require native validation before release support claims.

## Residual risks

- A malicious or mistaken model operation can cause arbitrary local data loss, credential disclosure, remote side effects, financial cost, or account compromise within the user's authority.
- No application-level design can keep owner-readable Morons state confidential from arbitrary processes running as the same user without an additional operating-system boundary.
- Same-user processes may interfere with sessions, files, IPC, child processes, attachments, or credential state.
- Cancellation and process-tree termination cannot undo filesystem changes, network requests, commits, pushes, publications, or processes that escaped ownership before termination.
- Concurrent sessions, task children, and ordinary user tools can race in the same working directory and invalidate prior observations or overwrite changes.
- Batched subagents can multiply provider cost and local side effects; hard application limits do not replace provider account budgets or user review.
- Child internal turns are not independently durable or resumable. A crash preserves only the outer uncertain task boundary and may leave provider usage or local effects without a child report.
- SQLite cannot atomically commit direct filesystem, process, provider, Git, or network effects.
- Context compaction is lossy and may omit relevant detail or preserve malicious instructions; canonical history remains available but is not all resent to the model.
- Old image pixels are not automatically resent after compaction, so exact later visual analysis may require reattachment.
- IPython kernel memory can disappear and should not be the only record of important state.
- Provider confidentiality, retention, availability, entitlement, pricing, and behavior remain external dependencies.
- Database and attachment confidentiality at rest depends on operating-system access controls and storage encryption and does not provide forensic erasure.
- Terminal emulators, accessibility tools, clipboard managers, screen capture, crash dumps, and same-user processes may observe displayed or pasted content.
- Native CI reduces but cannot eliminate operating-system, filesystem, terminal, clipboard, process-control, dependency, or hardware defects.
