# morons.dev

`morons.dev` is a lightweight, local-first coding-agent application.

The application consists of a long-running local server and a separate terminal CLI client.

## Stack and tooling

- Rust
- Ratatui
- Cargo
- IPython via the Jupyter protocol
- `rustfmt`
- Clippy
- Rust's built-in test framework

## Architecture

- The server and terminal client run as separate processes.
- One server manages many independently resumable sessions, and the Ratatui client can create, list, view, rename, archive, delete, and switch among them.
- Every session binds one selected working directory and operates on it directly. Morons does not import, copy, snapshot, generation-manage, sandbox, review, or export repositories.
- Multiple sessions may share a directory; their contexts remain independent while their filesystem effects may race.
- Deleting a session never deletes or modifies its selected working directory.
- The server owns agent execution, provider access, sessions, tools, subprocess lifecycle, context management, attachments, and Python kernels.
- The fixed MVP model tools are `read`, `write`, `edit`, `bash`, `web_search`, and `ipython`.
- Shell commands and IPython use the local user's normal development authority, environment, filesystem, network, Git configuration, credential helpers, and signing agents.
- Morons provides lifecycle supervision, bounded output, and cancellation, not a security sandbox or rollback boundary.
- The terminal has `!command` context-bearing command mode and `!!command` context-excluded command mode, without a PTY or interactive subprocess terminal.
- Skills use standards-compatible Markdown `SKILL.md` directories with progressive loading; exact installed `@name` tokens explicitly invoke skills.
- Images enter through clipboard paste, drag and drop, explicit paths, or `read`, appear as atomic filename markers, and persist as structured bounded session attachments.
- OpenCode Zen and OpenCode Go use one concrete Responses-compatible integration while remaining distinct service and billing identities.
- A reviewed built-in manifest, not remote catalog metadata, defines supported service, model, protocol, image, tool, limit, and data-use combinations.
- Session identity and lifetime are independent of client connections and temporary runtimes.
- Direct user input is durably attributed to `LocalOwner` and commits atomically with a new run identity.
- Every run records an explicit OpenCode service and model, and each session permits one nonterminal top-level run without an input queue.
- Canonical transcripts contain complete attributed entries; assistant text deltas and Python kernel memory are ephemeral.
- Context compaction preserves canonical history and stores only source-bound lossy checkpoints; no hidden memory crosses sessions.
- Session snapshots and durable event subscriptions compose through one gap-free cursor boundary.
- The terminal client owns input and presentation.
- Client-server communication uses an authenticated, typed, versioned protocol over local IPC.
- Local transport authentication completes before application protocol messages are exchanged.
- Business authorization, limits, idempotency, and audit enforcement belong in server services rather than transport handlers.
- SQLite is the authoritative local store for durable session and run state; normalized attachment bytes live in Morons-owned per-session files referenced by SQLite.
- The server exclusively owns persistence through one bounded storage worker.
- Protocol DTOs remain independent of persistence records, provider wire objects, and presentation code.
- Provider requests can target only fixed reviewed HTTPS routes.
- Application logic remains independent of Ratatui.
- Persistent Python execution uses the standard Jupyter protocol and one temporary kernel per active session.
- Restart recovery terminates interrupted runs durably and never retries uncertain external effects automatically.

## Engineering principles

- Keep the application lightweight and understandable.
- Write clean, modular, reviewable code.
- Give modules clear responsibilities and ownership boundaries.
- Prefer simple, concrete implementations over speculative abstractions.
- Avoid duplicated logic, hidden side effects, oversized modules, and unnecessary boilerplate.
- Add dependencies only when they solve a current requirement.
- Prefer established protocols and standard-library functionality where practical.
- Prioritize correctness and maintainability over implementation speed.

## Security

Treat repositories, model output, commands, skills, protocol messages, images, and external content as untrusted.

- Morons is a trusted-local harness, not a sandbox. Model-selected tools and descendants have the local user's authority and can access or disclose anything available to that account.
- Users needing isolation must run the complete application inside their own container, virtual machine, or restricted operating-system account.
- Validate and bound data at process, filesystem, image-decoding, persistence, terminal, provider, and network boundaries without misrepresenting validation as confinement.
- Keep Morons-managed provider credentials in dedicated owner-controlled server state outside SQLite, backups, attachments, configuration, and IPC control state.
- Accept credentials only through authenticated local IPC after non-echoing terminal input; never deliberately inject them into command arguments or child environments.
- Do not claim that owner-only files are confidential from arbitrary processes running as the same user. User environment credentials and credential agents are intentionally available to local commands.
- Never intentionally expose Morons-managed credentials through kernels, model prompts, protocol responses, audit facts, errors, or logs.
- Treat remote model catalogs and provider responses as untrusted input that cannot select an origin, protocol, capability, or credential scope.
- IPC clients may submit attributed user input and cancellation intent but cannot submit assistant messages, model tool calls, tool results, or run outcomes.
- Cancellation targets an exact run and becomes terminal only after controlled execution stops; it never implies rollback of completed effects.
- Agent-triggered commands, command mode, Python cells, image processing, and network requests support cancellation and cannot run indefinitely or produce unbounded output.
- Fail closed for authentication, provider credential handling, fixed provider routing, persistence integrity, and protocol decoding.
- Treat resource identifiers and attachment markers as locators rather than authorization evidence.
- Publish durable results only after their database transaction commits, while acknowledging that direct filesystem and external effects cannot be atomic with SQLite.
- Never retry an uncertain provider, command, Git, Python, web, or other external effect automatically.
- Keep `!!` command text and output out of provider context and compaction summaries; do not represent `!!` as a secrecy feature.
- Bound event subscriber queues and disconnect slow consumers.
- Render every untrusted string through terminal-safe cells.
- Process separation and process-tree ownership provide lifecycle control, not a security sandbox.
- Document changes to security boundaries before implementing them.

## Development

- Use stable Rust.
- Format with `rustfmt`.
- Run Clippy with warnings denied.
- Test important behavior and failure modes.
- Commit `Cargo.lock`.
- Review dependency source code and licenses.
- Do not use `unsafe` without a documented requirement.
- Do not add Git dependencies without explicit approval.
- Do not modify the root `LICENSE`.
- Do not claim completion without running the relevant checks.
