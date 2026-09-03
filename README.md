# morons.dev

A lightweight, local-first coding-agent CLI built in Rust.

Morons works directly in the directory where you start it. It keeps durable sessions in a local companion server and provides a small model tool set: `read`, `write`, `edit`, `bash`, `web_search`, and persistent-session `ipython`.

## Security model

**Morons is not a sandbox.** Model-selected file operations, Bash commands, Python cells, dependencies, Git operations, and network requests run with your normal operating-system user authority. They can access, change, delete, or disclose anything your account can access. There are no approval prompts or rollback, and cancellation cannot undo effects that already completed.

If you need containment, run the **complete application**—client, companion server, child processes, kernels, state, credentials, and selected directory—inside a container, virtual machine, or restricted operating-system account that you configure and trust.

Do not use production credentials with untrusted repositories unless you accept this authority model.

## Build and run

Requirements:

- Rust 1.98 (selected by `rust-toolchain.toml`)
- a Bash-compatible shell; Windows requires Git Bash or compatible Bash configuration
- an OpenCode Zen or Go API key
- Python with `jupyter_client` and `ipykernel` for the `ipython` tool
- `BRAVE_SEARCH_API_KEY` for `web_search`

Build both packaged companions:

```sh
cargo build --locked --release -p morons-cli -p morons-server
```

Keep the resulting `morons` and `morons-server` executables together, change to the directory you want a new session to use, and run:

```sh
./target/release/morons
```

On first launch, read and acknowledge the trusted-local authority notice. Configure the OpenCode credential with `Ctrl+K`.

## Interaction

- `Enter`: submit a message
- `Shift+Enter`: insert a newline
- `@name`: activate an installed Agent Skill
- `!command`: execute bounded noninteractive Bash and include its command/result in later model context
- `!!command`: execute Bash but exclude its command/result from model context
- `/context`: inspect approximate context use, limits, reserves, and the latest checkpoint
- `/compact [instructions]`: manually summarize an eligible old context prefix
- `r` in the session browser: rename the selected durable session
- `Tab` / `Shift+Tab`: select models or complete skills
- `Ctrl+V` (`Alt+V` on Windows): paste an image when available
- `Ctrl+X`: cancel the selected session's active run or command
- `Esc`: return to the session browser without cancelling server-owned work
- `?` or `/help`: show usage and the security disclosure
- `Ctrl+S`: stop the companion server and interrupt active work

Commands are noninteractive: standard input is closed, no PTY is provided, and output and runtime are bounded. `bash` and `ipython` still have your ordinary filesystem, environment, network, Git, credential-helper, and agent access.

## Sessions and context

Each session is durably bound to one absolute working directory. Switching sessions or closing the client does not cancel server-owned work. Multiple sessions may use the same directory, so their filesystem effects can race even though their histories are independent.

Canonical transcript history remains durable. Automatic and manual compaction create source-bound lossy summaries for provider context without deleting canonical messages or image attachments. `!!` content is never included in provider context or summaries.

## Skills

Morons reads standard `SKILL.md` directories from bundled, user, and project roots. Exact standalone `@name` tokens activate installed skills. Skills and their resources are untrusted instructions with the same tool authority as any other repository content.

## Platforms

The intended targets are x86_64 and aarch64 on macOS, Linux, and Windows. CI exercises Linux, macOS, and Windows plus Linux and Windows aarch64 coverage. Native Intel macOS validation remains required before claiming release support for that target; see [ADR 0007](docs/adr/0007-supported-processor-architectures.md).

## License

Apache-2.0. See [LICENSE](LICENSE).
