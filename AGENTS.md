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
- The server owns agent execution, sessions, tools, subprocesses, and Python kernels.
- Session identity and lifetime are independent of client connections and temporary runtimes.
- Each session owns an isolated mutable workspace and execution context.
- The terminal client owns input and presentation.
- Client-server communication uses an authenticated, typed, versioned protocol over local IPC.
- Local transport authentication completes before application protocol messages are exchanged.
- Business authorization, limits, idempotency, and audit enforcement belong in server services rather than transport handlers.
- Protocol DTOs must remain independent of persistence records and presentation code.
- Application logic must remain independent of Ratatui.
- Python execution and PTY command execution are separate subsystems.
- Persistent Python execution uses the standard Jupyter protocol.
- Kernel memory is temporary working state, not authoritative session storage.

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
- Keep provider credentials in the server and local IPC authentication material in owner-controlled state.
- Never expose credentials through kernels, untrusted subprocesses, prompts, or logs.
- Agent-triggered commands, Python cells, and network requests must support cancellation and must not run indefinitely or produce unbounded output.
- Fail closed for security-sensitive behavior.
- Treat resource identifiers as locators rather than authorization evidence.
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
