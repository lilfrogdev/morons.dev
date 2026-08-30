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
- One server manages many independently resumable sessions.
- The server owns agent execution, provider access, sessions, tools, subprocesses, and Python kernels.
- OpenCode Zen and OpenCode Go use one concrete Responses-compatible integration while remaining distinct service and billing identities.
- A reviewed built-in manifest, not remote catalog metadata, defines supported service, model, protocol, capability, and data-use combinations.
- Session identity and lifetime are independent of client connections and temporary runtimes.
- Each session owns an isolated mutable workspace and execution context.
- The terminal client owns input and presentation.
- Client-server communication uses an authenticated, typed, versioned protocol over local IPC.
- Local transport authentication completes before application protocol messages are exchanged.
- Business authorization, limits, idempotency, and audit enforcement belong in server services rather than transport handlers.
- SQLite is the authoritative local store for durable session and run state.
- Canonical history is append-only, while current-state and delivery views are transactional, rebuildable projections.
- The server exclusively owns persistence through one bounded storage worker.
- Protocol DTOs must remain independent of persistence records, provider wire objects, and presentation code.
- Provider requests can target only fixed reviewed HTTPS routes.
- Application logic must remain independent of Ratatui.
- Python execution and PTY command execution are separate subsystems.
- Persistent Python execution uses the standard Jupyter protocol.
- Kernel memory is temporary working state, not authoritative session storage.
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

Treat repositories, model output, commands, skills, protocol messages, and external content as untrusted.

- Validate data at process, filesystem, and network boundaries.
- Keep provider credentials in dedicated owner-controlled server state outside SQLite, backups, workspaces, configuration, and IPC control state.
- Accept credentials only through authenticated local IPC after non-echoing terminal input; never accept them from command arguments or environments.
- Never expose credentials through kernels, untrusted subprocesses, model prompts, protocol responses, audit facts, errors, or logs.
- Treat remote model catalogs and provider responses as untrusted input that cannot select an origin, protocol, capability, or credential scope.
- Agent-triggered commands, Python cells, and network requests must support cancellation and must not run indefinitely or produce unbounded output.
- Fail closed for security-sensitive behavior.
- Treat resource identifiers as locators rather than authorization evidence.
- Keep the database, backups, and other sessions' workspaces inaccessible to untrusted execution.
- Publish durable results only after their database transaction commits.
- Bound event subscriber queues and disconnect slow consumers.
- Keep security enforcement in trusted server code.
- Process separation provides lifecycle isolation, not a security sandbox.
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
