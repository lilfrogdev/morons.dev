# morons.dev

A lightweight, local-first coding-agent CLI built in Rust.

Morons works directly in the directory where you start it. It keeps durable sessions in a local companion server and provides a small model tool set: `read`, `write`, `edit`, `bash`, `web_search`, persistent-session `ipython`, and bounded batched `task` subagents.

## Security model

**Morons is not a sandbox.** Model-selected file operations, Bash commands, Python cells, dependencies, Git operations, and network requests run with your normal operating-system user authority. They can access, change, delete, or disclose anything your account can access. There are no approval prompts or rollback, and cancellation cannot undo effects that already completed.

If you need containment, run the **complete application**—client, companion server, child processes, kernels, state, credentials, and selected directory—inside a container, virtual machine, or restricted operating-system account that you configure and trust.

Do not use production credentials with untrusted repositories unless you accept this authority model.

## Build and run

Requirements:

- Rust 1.98 (selected by `rust-toolchain.toml`)
- a Bash-compatible shell; Windows requires Git Bash or compatible Bash configuration
- an OpenCode Zen or Go API key
- internet access for the first managed IPython setup and for `web_search`
- `BRAVE_SEARCH_API_KEY` for `web_search`

Build the Rust client and server companion:

```sh
cargo build --locked --release -p morons-cli -p morons-server
```

From a clean checkout, create a checksummed archive for the current Rust host target with:

```sh
./scripts/package-release.sh
```

Pass one of the six reviewed target triples as the first argument when its Rust target and linker are available.

Keep the resulting `morons`, `morons-server`, and `morons-uv` executables together. From a source checkout, change to the directory you want a new session to use and launch the client by path:

```sh
cd /path/to/project
/path/to/morons.dev/target/release/morons
```

From an extracted release archive, launch its client the same way without moving or renaming its sibling companion:

```sh
cd /path/to/project
/path/to/extracted-morons-package/morons
```

Before installing an archive, verify it against the release's `SHA256SUMS`. Keep all three executables in one owner-controlled directory that is not writable by other users. You may add that directory to `PATH`, but invoke only `morons`; `morons-server` and `morons-uv` are internal companions.

To update, stop the running companion with `Ctrl+S`, verify and extract the new archive, replace all three executables before relaunching, and never mix companions from different versions. Database migrations are forward-only; downgrading an existing state directory is unsupported.

On first launch, read and acknowledge the trusted-local authority notice. Configure the OpenCode credential with `/login`; `Ctrl+K` opens the same dialog as a shortcut. Maintainers follow [the release procedure](docs/releasing.md) and [release-candidate QA checklist](docs/release-candidate-qa.md).

### Managed IPython runtime

Release archives include a checksummed `morons-uv` helper. On the first `ipython` call, the companion uses it to prepare Morons-owned Python 3.11.15 with hash-locked `jupyter_client` 8.6.3 and `ipykernel` 6.30.1. Initial setup requires internet access to the reviewed Python and PyPI sources. The versioned runtime and download cache live under `~/.morons/python` on macOS/Linux or `%LOCALAPPDATA%\\morons.dev\\python` on Windows; later use works without network access. Interrupted, stale, or invalid staging state is rebuilt under a process lock and never becomes the active runtime.

Normal use does not require Python or `pip` to be installed. `MORONS_PYTHON` remains an expert override: when set before the companion starts, Morons bypasses managed setup and uses that executable, which must provide `jupyter_client` and `ipykernel`. Stop an existing companion with `Ctrl+S` before changing the override.

Direct source-tree binaries do not automatically download build companions. Maintainers can use `scripts/package-release.sh` to produce a complete local archive; developers intentionally testing an existing Python may continue to set `MORONS_PYTHON`.

## Interaction

- `Enter`: submit a message
- `Shift+Enter`: insert a newline
- Mouse wheel/trackpad or `PageUp`/`PageDown`: scroll the fullscreen transcript
- `Home`/`End`: jump to the start/latest transcript output
- `@name`: activate an installed Agent Skill
- `!command`: execute bounded noninteractive Bash and include its command/result in later model context
- `!!command`: execute Bash but exclude its command/result from model context
- `/model [search]`: search available reviewed models and save one global default for every session
- `/login`: configure, replace, or remove the OpenCode API credential through hidden input (`Ctrl+K` shortcut)
- `/context`: inspect approximate context use, limits, reserves, and the latest checkpoint
- `/compact [instructions]`: manually summarize an eligible old context prefix
- `r` in the session browser: rename the selected durable session
- `a` in the session browser: archive or unarchive the selected session
- `d` in the session browser: delete an archived session's Morons-owned history and attachments after confirmation; the working directory is never changed
- `Tab` / `Shift+Tab`: complete or navigate visible skill matches
- `Ctrl+V` (`Alt+V` on Windows): paste an image when available
- `Ctrl+X`: cancel the selected session's active run or command
- `Esc`: return to the session browser without cancelling server-owned work
- `?` or `/help`: show usage and the security disclosure
- `Ctrl+S`: stop the companion server and interrupt active work

Commands are noninteractive: standard input is closed, no PTY is provided, and output and runtime are bounded. `bash` and `ipython` still have your ordinary filesystem, environment, network, Git, credential-helper, and agent access.

## Sessions and context

Each session is durably bound to one absolute working directory. Switching sessions or closing the client does not cancel server-owned work. Multiple sessions may use the same directory, so their filesystem effects can race even though their histories are independent.

The most recently selected or used reviewed model is the global default across sessions and client restarts. Opening an older session does not restore that session's historical model. If the saved default is unavailable, Morons uses another currently available reviewed model and reports the fallback.

Canonical transcript history remains durable. The terminal opens at the newest bounded window and pages older or newer history on demand, rendering only visible transcript blocks during steady-state frames. Automatic and manual compaction create source-bound lossy summaries for provider context without deleting canonical messages or image attachments. `!!` content is never included in provider context or summaries.

Every OpenCode Zen and Go inference request carries one stable, derived `x-opencode-session` identifier for its Morons conversation. The root value remains constant across the durable session's runs, compaction, and tool turns. Each task child receives a distinct value stable across its own turns. These identifiers are not sent on public model-catalog requests.

## Subagents

The `task` tool follows a bounded OMP-style batch contract: the parent supplies shared context once and one to three self-contained assignments. Children run concurrently with the parent's current model, receive only `read`, `write`, `edit`, `bash`, and `web_search`, and return input-ordered bounded reports. They do not inherit the parent transcript, share IPython memory, recurse, continue in the background, or receive isolated worktrees. Children share the real selected directory, so parallel mutations can race.

## Skills

Morons reads standard `SKILL.md` directories from bundled, user, and project roots. Exact standalone `@name` tokens activate installed skills. Skills and their resources are untrusted instructions with the same tool authority as any other repository content.

## Platforms

The intended targets are x86_64 and aarch64 on macOS, Linux, and Windows. CI exercises Linux, macOS, and Windows plus Linux and Windows aarch64 coverage. Native Intel macOS validation remains required before claiming release support for that target; see [ADR 0007](docs/adr/0007-supported-processor-architectures.md).

## License

Apache-2.0. See [LICENSE](LICENSE).
