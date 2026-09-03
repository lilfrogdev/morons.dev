# ADR 0012: Trusted-local direct-workspace MVP

## Status

Accepted.

This decision supersedes ADRs 0008, 0009, 0010, and 0011. It also supersedes the private-workspace, sandbox-only execution, controlled-export, and no-command-mode portions of ADRs 0002, 0003, 0005, and 0006. The remaining local IPC, application-service, durable-session, provider, credential, terminal-lifecycle, and processor-portability decisions continue to apply where they do not conflict with this decision.

## Context

Morons is intended to be a lightweight local coding-agent harness. Its useful authority comes from working in the user's real development directory with the tools, languages, network, Git configuration, signing agents, and credentials already available to that user.

ADRs 0008 through 0011 instead made Morons a repository-import and sandbox-publication system. A session copied a selected repository into an immutable baseline and mutable generations, admitted only structured worktree tools, provisioned server-owned execution images, ran commands in native sandboxes, reviewed a custom diff, and exported a new tree through a controlled publication operation. That design added substantial code and platform machinery while preventing ordinary local workflows such as installing dependencies, using an existing language environment, signing a commit, pushing a branch, or creating a pull request.

Process separation still provides useful lifecycle ownership: the server can keep sessions and runs alive while terminal clients detach, retain provider credentials, supervise child processes, and recover durable history. It does not make a process running as the local user a security sandbox. A model-selected command running with the user's authority can read, modify, delete, or disclose anything that user can access.

The MVP therefore needs an explicit trusted-local posture, direct selected directories, a small tool set, durable multimodal sessions, progressive Markdown skills, bounded context management, command mode, and ordinary local Git workflows. Users who need containment must put the complete Morons application inside a container, virtual machine, or restricted operating-system account that they configure and trust.

## Decision

### Product and trust posture

Morons is a trusted-local coding-agent harness, not a security sandbox, IDE, terminal emulator, repository publication service, or remote control plane.

Repository content, model output, skills, web content, images, and process output remain untrusted data. However, once the model selects a filesystem mutation, Python cell, or shell command, that operation runs with the authority of the local user. Morons does not attempt to confine that authority, emulate a permission boundary with path validation, or imply that cancellation can roll back completed effects.

The application displays this posture during onboarding and makes it available in help. There are no per-command approval prompts by default. Optional containment is entirely external to Morons and must wrap the client, companion server, child processes, kernels, data directories, credentials, and selected working directory together.

Morons does not add a user-facing PTY, interactive subprocess terminal, SSH server, raw server console, editor, marketplace runtime, arbitrary privileged proxy, or plugin execution framework. Shell commands are noninteractive operations with closed standard input and bounded captured output.

Subagents are outside the MVP. A later decision may add independently bounded contexts and concurrent subagents that share the selected working directory. It must not reintroduce managed worktrees merely to provide subagent isolation.

### Direct working directories

Every session binds one user-selected working directory. Running `morons` in a directory proposes the process working directory by default, and the user may select another directory when creating a session.

The server stores the absolute working-directory path as session metadata because it is required to resume direct work. The path is a locator, not authorization evidence. Before each run or local command, Morons verifies that it still resolves to a directory and reports a clear unavailable-directory state when it does not. The MVP does not silently retarget a session; selecting another directory creates another session.

Tools and child processes operate directly on the selected directory. There is no repository import, copied baseline, private worktree, generation pointer, candidate promotion, custom diff authority, or export operation. Changes are visible immediately to the user and ordinary development tools. The directory need not be a Git repository and may contain any language or toolchain supported by the user's environment.

Relative tool paths resolve from the session working directory. Absolute paths and normal operating-system path semantics are allowed because shell and Python execution already have the user's filesystem authority. Path validation prevents malformed inputs and accidental implementation bugs; it is not represented as confinement. Operating-system permissions remain authoritative.

Several durable sessions may bind the same directory. Session history and contexts remain independent, but filesystem state does not. Morons shows that the directory is shared and warns when concurrent runs may race. It does not claim cross-session filesystem isolation or rollback.

### Multiple durable sessions

The Ratatui client retains a session browser for creating, listing, viewing, resuming, renaming, archiving, deleting, and switching sessions. Each session has independent transcript history, context checkpoints, attachments, model selections, run state, and temporary IPython runtime.

Switching or closing a client detaches presentation only. It does not cancel an active run, terminate another session, or stop the server. The browser shows bounded status for active, idle, interrupted, failed, and unavailable-directory sessions. One session permits at most one nonterminal top-level run, while different sessions may run concurrently within global limits.

Deleting a session deletes only Morons-owned session records, attachments, and temporary runtime state. It never deletes, resets, cleans, checks out, or otherwise modifies the selected working directory.

### Core tools

The MVP model tool catalog contains exactly these small built-ins:

- `read`: read bounded text from a file or return a normalized common-format image;
- `write`: write one complete bounded file, creating or replacing it deliberately;
- `edit`: apply bounded exact, unique, nonoverlapping text replacements;
- `bash`: execute one bounded noninteractive shell command in the selected directory;
- `web_search`: return bounded cited search results through a reviewed search adapter; and
- `ipython`: execute a bounded cell in the session's persistent IPython kernel.

Tool names are stable provider-facing names. Inputs and results use strict bounded schemas. Tool output is committed before it is used in a later provider turn. Tool definitions, prompts, and model annotations are usability contracts, not security boundaries.

`web_search` uses Brave Search's fixed reviewed HTTPS API endpoint, does not follow redirects or accept a model-selected origin, and returns only bounded titles, URLs, and snippets. The user supplies `BRAVE_SEARCH_API_KEY` in Morons' ordinary process environment. Morons does not persist or log that value, but the trusted-local execution posture means it is also available to child commands just like the user's other environment credentials.

`read`, `write`, and `edit` use direct filesystem semantics. They reject malformed encodings, oversized inputs, ambiguous edits, and impossible operations, but they do not claim to prevent the model from reaching user-accessible paths. `bash` and `ipython` make any path-only restriction unenforceable and would turn such a restriction into misleading security theater.

The shell tool uses a compatible Bash executable. On Windows, release support requires a documented compatible Bash installation or explicit shell configuration; Morons does not silently reinterpret Bash commands as PowerShell. The active shell and operating system are stated in model context.

Every shell invocation starts in the session working directory, uses the user's normal development environment, has closed standard input, captures standard output and error separately, and receives bounded output, inactivity, and wall-clock limits. Cancellation, timeout, client-requested stop, and server shutdown terminate the complete owned process tree. Successful effects that occurred before termination remain ordinary filesystem or external effects and are not rolled back.

The trusted companion itself still starts from an exact packaged path without a shell. Normal development environment values supplied to child execution are bounded, transient, and excluded from persistence, logs, audit records, model prompts, and provider requests unless a command prints them. Morons does not inject its stored OpenCode credential into children. Credentials independently present in the user's environment, home directory, keychain, credential helper, or agent remain available to those children by design.

The server-owned provider credential boundary is an application boundary, not an operating-system confidentiality boundary against processes running as the same user. Without external containment, a malicious same-user process may inspect Morons-owned files or authenticate as the local owner. Morons must not claim otherwise.

### Persistent IPython

Each active session may start one IPython kernel on demand through the standard Jupyter protocol. Morons launches the configured Python runtime (`MORONS_PYTHON`, or the platform default), which must provide `jupyter_client` and `ipykernel`, and supervises the bridge, kernel, and descendants as one process tree. The kernel starts in the selected working directory with the same intentional local-authority posture as shell commands. It may import packages, access the network, launch subprocesses, and modify user-accessible files.

Kernel variables persist across cells and top-level runs while that kernel remains alive. Kernel memory is temporary and is never authoritative session state; server restart, cancellation, limit exhaustion, least-recently-used idle-kernel eviction, or unrecoverable kernel failure loses it. Morons keeps at most four session kernels alive, terminates the complete kernel process tree when an operation cannot finish safely, and starts a fresh kernel on the next cell. The durable transcript retains only bounded submitted cells and displayed results.

Morons exposes a small Python helper surface that can invoke the same built-in tools programmatically. Those calls retain their ordinary typed inputs, limits, cancellation, transcript ordering, and provider-context behavior. The helper is not a generic privileged server proxy.

### Skills and `@` invocation

Morons implements the Agent Skills `SKILL.md` format. A skill is a directory whose `SKILL.md` contains required `name` and `description` YAML frontmatter followed by Markdown instructions. Optional scripts, references, and assets remain ordinary files relative to the skill directory.

The MVP discovers skills from these roots:

- bundled Morons skills;
- `~/.morons/skills/` and `~/.agents/skills/`; and
- `.morons/skills/` and `.agents/skills/` in the selected directory and applicable ancestor directories up to the Git root or filesystem root.

Project skills override user skills of the same name, user skills override bundled skills, and collisions at the same precedence are reported rather than selected nondeterministically. Discovery is bounded and validates the Agent Skills name and description rules.

Only bounded skill names, descriptions, sources, and exact `SKILL.md` locators are included in normal model context. A model progressively loads a filesystem skill through `read`. When the user invokes a standalone whitespace-delimited exact installed `@name` token, the server captures the complete bounded `SKILL.md` into that run's durable context snapshot before dispatch. Typing `@` opens bounded skill completion. Unknown `@` tokens remain ordinary prompt text so email addresses, package names, and usernames are not rewritten.

Morons reserves `@` for skills and does not use it for file references. Files use ordinary paths, drag and drop, clipboard attachment, or the `read` tool. A bundled `skill-creator` skill can create or refine standards-compatible skills in a user- or project-selected skill root using ordinary file tools. Discovery parses bounded YAML 1.2 frontmatter without following skill-directory symlinks, requires standard names to match parent directories, applies deterministic project-over-user-over-bundled precedence, and makes same-precedence name collisions unavailable with a local warning.

Skills are instructions and ordinary executable resources, not trusted extensions or capabilities. They can direct the model to use every authority already available to tools.

### Local command mode

A prompt beginning with one `!` executes the remaining text as a local shell command instead of sending that text as a user request:

- `!command` executes locally, displays bounded output, and adds the command and bounded result to subsequent model context.
- `!!command` executes locally and displays bounded output, but excludes both command and output from provider context and compaction summaries.

Command mode uses the session working directory, shell, environment, cancellation, output, deadline, and process-tree contract used by `bash`. It is available only while that session has no active run, so a command-mode effect cannot race the same session's model tool loop. It is noninteractive and provides no PTY.

Completed and interrupted command-mode operations are durable local-command transcript entries with an explicit context-visibility flag. This preserves session viewing and resume without confusing a local command with user or model authorship. Context construction includes `!` entries and always excludes `!!` entries. `!!` controls model context only; it is not a secrecy, filesystem-isolation, process-isolation, persistence-confidentiality, or terminal-confidentiality feature.

### Multimodal image input

The MVP accepts images through all of these paths:

- image data read from the system clipboard;
- drag-and-drop paths delivered by supported terminals;
- explicit image paths in user input; and
- image files opened by the `read` tool.

Clipboard paste is image-first and falls back to clipboard text. The default image binding is `Ctrl+V`, with `Alt+V` on Windows where terminals commonly consume `Ctrl+V`. Morons also handles supported bracketed-paste and drag-and-drop path forms conservatively, including quoted, escaped, `file://`, Windows, and paths containing spaces.

An attachment is structured state separate from prompt text. The editor renders one atomic filename marker at the insertion point, such as `[puppies.png]`. File and drag attachments preserve a sanitized basename. Clipboard images without a source filename use deterministic draft names such as `[pasted-image-1.png]`; duplicate display names receive a stable numeric suffix. The filename marker is presentation and prompt-order metadata, not the image payload or an authoritative filesystem path.

Morons captures attachment bytes immediately so temporary pasteboard and drag paths are not required after ingestion. It detects type from content, applies image orientation, validates dimensions, converts supported non-provider formats to PNG when necessary, and normalizes PNG, JPEG, WebP, and GIF within conservative dimension and encoded-payload limits. Each attachment records its filename, media type, original and normalized dimensions, size, and session-owned storage reference.

Normalized attachment bytes are stored once in Morons-owned per-session attachment state and referenced by durable user-message entries. Provider adapters map provider-neutral text and image parts to the selected model's wire format. If the selected model lacks image input, submission fails clearly while retaining the draft and attachments; images are never silently discarded.

Inline terminal image rendering is not required. Filename markers and attachment metadata are sufficient for the MVP. Attachment input, decoding, processing, storage, provider serialization, replay, and terminal rendering each apply independent count and size limits.

### Context management

Complete bounded transcript history remains durable. Context compaction is a lossy projection and never deletes, rewrites, or replaces canonical user messages, assistant messages, attachment references, tool calls, or tool results.

Before each provider dispatch, a deterministic versioned context policy assembles:

1. Morons developer instructions and tool contracts;
2. the selected working directory and applicable project instructions;
3. bounded skill names and descriptions;
4. complete instructions for skills active in the current request;
5. the newest valid compaction checkpoint, if present;
6. the uncompacted recent transcript in canonical order; and
7. the current text and structured image attachments.

Morons reconstructs this context locally. Provider response identifiers and provider-hosted conversation state are never authoritative session memory. Opaque reasoning continuation may remain transient only while required by one live provider tool loop.

The policy reserves model-specific space for output, tool schemas, and bounded tool-loop growth. When projected input crosses the versioned compaction threshold, Morons asks the selected model to summarize the oldest completed prefix while retaining the current run and a recent tail verbatim. The checkpoint records the exact source-entry high water and digest, policy version, bounded summary, token estimate, and lineage. A failed or invalid compaction leaves canonical history unchanged.

A compaction summary preserves the user's goal, requirements, decisions, constraints, relevant files and changes, commands and tests, errors, visual observations, and remaining work. Old image bytes covered by a checkpoint remain in session history but are not resent automatically; relevant visual findings survive in the summary. If the current run alone cannot fit, the run fails with a clear context-limit result rather than silently discarding current information.

Tool and command results are bounded before entering history or context. `!!` commands never enter context or compaction. IPython kernel memory does not enter context unless a bounded cell result states it explicitly. Context summaries are untrusted model output and never authorize an operation or override developer instructions.

The Ratatui client provides `/context`, `/compact`, and `/compact <instructions>`. `/context` reports approximate current use, model limit, reserved capacity, and the latest checkpoint. The status view shows an approximate context meter. Compaction thresholds and estimators are versioned server policy rather than user-tunable MVP settings.

No embeddings, vector database, cross-session memory, automatic project memory, session branching, or subagent context exists in the MVP.

### Ordinary Git and development workflows

Git is not a trusted built-in service and Morons does not implement custom repository import, diff, commit, signing, push, pull-request, or export protocols. The model uses normal commands through `bash`, including `git diff`, `git commit -S`, `git push`, and `gh pr create` when those programs and credentials are available in the user's environment.

Git hooks, signing agents, SSH agents, credential helpers, network access, repository configuration, dependencies, and generated processes execute with the user's normal authority. Morons supplies no special Git or GitHub credential and no privileged proxy. The user reviews changes through their ordinary tools and repository host.

### Persistence and recovery

SQLite remains authoritative for session metadata, selected working-directory locators, bounded canonical transcripts, run state, run-bound skill catalogs and active instruction snapshots, attachment references, context checkpoints, idempotent client mutations, and durable subscription cursors. Attachments are Morons-owned files referenced from SQLite rather than repeated base64 payloads.

Canonical transcript entries include attributed user messages, completed assistant messages, typed tool calls and results, and bounded local command-mode entries whose context-visibility field distinguishes `!` from `!!`. Persistence records these ordinary completed or interrupted operations but does not attempt to make direct filesystem or external command effects atomic with SQLite. A crash can leave a command's effects applied without a terminal transcript result. Startup terminates nonterminal runs as interrupted, does not rerun provider or tool work, and does not claim rollback or infer filesystem state.

Direct local effects do not create the old workspace-generation uncertainty blocker because there is no authoritative private generation to reconcile. The user and a later model inspect the actual selected directory with ordinary tools. External side effects are never retried automatically after an unknown outcome.

The MVP retains one nonterminal top-level run per session, exact-run cancellation, background execution independent of client attachment, bounded subscriptions, complete assistant-message commits, and ephemeral text deltas. It prefers ordinary session and transcript records over extending full fact/projection machinery to every local operation.

## Implementation sequence

The architecture reset is implemented in narrow changes:

1. remove repository import, immutable baselines, worktree generations, execution images, native sandbox helpers, custom review, and controlled export;
2. simplify sessions to bind a direct selected working directory while retaining multi-session browsing, persistence, subscriptions, provider custody, and cancellation;
3. implement direct `read`, `write`, `edit`, and normal-environment `bash` tools;
4. add command mode and complete process-tree lifecycle controls;
5. add persistent session IPython and bounded `web_search`;
6. add Agent Skills discovery, `@` invocation, and the bundled skill creator;
7. add durable clipboard, drag-and-drop, path, and `read` image input plus provider multimodal mapping; and
8. add automatic and manual context compaction and context status.

Each removal or addition receives the narrowest relevant unit, integration, platform, and security-boundary tests. Native release claims continue to require the target coverage selected in ADR 0007.

## Consequences

- Morons can work immediately in an existing project with its real languages, tools, dependencies, Git metadata, credentials, and network.
- The implementation can remove the repository-copy, sandbox, generation, custom review, and export architecture.
- Direct effects are simpler and more useful but cannot be rolled back or reconciled as isolated workspace generations.
- A malicious or mistaken model command has the same practical authority as a command typed by the user.
- Provider and IPC custody remain useful application boundaries but cannot protect owner-readable state from arbitrary processes running as that owner.
- Multiple durable sessions and background runs remain available, including sessions sharing one directory with explicit race risk.
- Context compaction preserves long-running usability without destroying canonical history.
- Skills and image input remain portable, progressively loaded, and provider-neutral.
- Users requiring meaningful containment must supply it outside Morons.

## Alternatives rejected

- Continuing the ADR 0008 through 0011 architecture would preserve an isolation claim at the cost of ordinary development workflows and substantial platform complexity.
- Optional built-in sandbox modes would preserve two incompatible execution and recovery architectures and imply a portability guarantee Morons does not need for the MVP.
- Copying repositories or creating Git worktrees per session would make existing uncommitted state, hooks, credentials, and direct user collaboration surprising.
- Approval prompts do not create a security boundary and would interrupt the intended autonomous local workflow.
- Restricting structured file tools to the selected directory while unrestricted shell and Python execution remain available would be misleading rather than protective.
- Provider-hosted conversation state would make durable local recovery depend on an external opaque continuation.
- Deleting old transcript entries during compaction would make a lossy model summary authoritative history.
- A proprietary skill database or executable plugin system would reduce compatibility and enlarge the trusted surface.
- A terminal emulator, PTY, or editor would move Morons away from a small agent harness and duplicate ordinary development tools.
