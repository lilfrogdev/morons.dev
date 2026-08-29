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
- The server owns agent execution, sessions, tools, subprocesses, and Python kernels.
- The terminal client owns input and presentation.
- Client-server communication uses an authenticated, typed, versioned protocol.
- Local transport authentication completes before application protocol messages are exchanged.
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
