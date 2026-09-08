# morons.dev

A lightweight, local-first coding-agent CLI built in Rust.

Morons works directly in the directory where you start it. It keeps durable sessions in a local companion server and provides a small model tool set: `read`, `write`, `edit`, `bash`, `web_search`, persistent-session `ipython`, and bounded batched `task` subagents.

## Security model

**Morons is not a sandbox.** Model-selected file operations, Bash commands, Python cells, dependencies, Git operations, and network requests run with your normal operating-system user authority. They can access, change, delete, or disclose anything your account can access. There are no approval prompts or rollback, and cancellation cannot undo effects that already completed.

If you need containment, run the **complete application**—client, companion server, child processes, kernels, state, credentials, and selected directory—inside a container, virtual machine, or restricted operating-system account that you configure and trust.

Do not use production credentials with untrusted repositories unless you accept this authority model.

## Build and run

Source-build requirements:

- Rust 1.98 (selected by `rust-toolchain.toml`)
- a Bash-compatible shell

Packaged use does not require Rust or a preinstalled Python. Bash is required for the `bash` tool and `!`/`!!` command modes; on Windows, Morons discovers a normal Git for Windows installation or uses the expert `MORONS_BASH` override set before the companion starts. Model inference requires the selected provider's credential: an OpenCode Zen/Go API key or native ChatGPT login. Neither is required for launch or local session management. First managed-IPython setup and provider operations require network access. Successful `web_search` additionally requires `BRAVE_SEARCH_API_KEY` in the companion's inherited environment.

Build the Rust client and server companion:

```sh
cargo build --locked --release -p morons-cli -p morons-server
```

From a clean checkout, create a checksummed archive for the current Rust host target with:

```sh
./scripts/package-release.sh
```

Pass one of the six reviewed target triples as the first argument when its Rust target and linker are available.

Keep the complete extracted package together; in particular, `morons`, `morons-server`, and `morons-uv` must remain exact siblings. Executable names have an `.exe` suffix on Windows. From a source checkout, change to the directory you want a new session to use and launch the client by path:

```sh
cd /path/to/project
/path/to/morons.dev/target/release/morons
```

From an extracted release archive, launch its client the same way without moving or renaming its sibling companion:

```sh
cd /path/to/project
/path/to/extracted-morons-package/morons
```

Before installing an archive, verify it against the release's `SHA256SUMS` and inspect its `MANIFEST.txt`. Keep the complete package in one owner-controlled directory that is not writable by other users. You may add that directory to `PATH`, but invoke only `morons`; `morons-server` and `morons-uv` are internal companions.

To update, stop the running companion with `Ctrl+S`, verify and extract the new archive into a new complete installation directory, and launch that directory's `morons`. Do not copy individual executables over an old package or mix companions from different versions. Durable state remains in the application state directory and migrates forward on the next start. Database migrations are forward-only; downgrading an existing state directory is unsupported.

On every client launch, read and acknowledge the trusted-local authority notice. `/login` or `Ctrl+K` chooses OpenCode or ChatGPT. OpenCode API-key input remains non-echoing. After explicit confirmation, ChatGPT login automatically opens your default browser. If needed, click **Open browser** (`o`/Enter) or **Copy link** (`c`) to copy the complete URL for pasting into your browser; terminal-dependent text selection is not required. Clipboard managers may retain copied links. Never paste callback codes or tokens into Morons. If the browser reports a rejected callback, share only its fixed reason—not the callback URL—and do not refresh/replay that callback. A code-received browser page is not login success: Morons must save the credential. If token validation fails, report only Morons' fixed rejection reason, never tokens or response bodies. Esc requests cancellation, but a save already started may complete. **ChatGPT models are explicitly selectable: GPT-6 Astra, GPT-5.6 Sol/Luna/Terra, Daybreak Blue, and GPT-5.5.** Daybreak Blue requires provider approval/provisioning; selection does not prove account access. Native inference interoperability remains unqualified. A native protocol failure can show a fixed rejection reason and local run locator while connected; share only that reason, not provider responses or credentials. This status is ephemeral and never implies retry permission. See [the reviewed model contract](docs/adr/0035-reviewed-native-model-expansion.md). Each output limit is local validation, not a remote generation or spending cap; remote work may already exceed it. Unknown account-controlled data policy cannot satisfy either restrictive setting. Login never changes the selected model. `/logout` chooses a provider for confirmed local removal; remote authorization and dispatched work may remain. Revoke access through the provider account when needed. Credentials live in dedicated owner-controlled state outside SQLite and are never intentionally exposed to tools or kernels. Maintainers follow [the release procedure](docs/releasing.md) and [release-candidate QA checklist](docs/release-candidate-qa.md).

### Data-use restrictions

Both `/settings` restrictions default **off** and are independent. They use reviewed manifest metadata, not account claims: unknown/account-controlled policy cannot satisfy a restriction. Blocked models remain visible and selected without fallback, but new selections, inputs and provider dispatches (including children and compaction) are checked by the server. Policy changes invalidate stale background summaries before dispatch/installation; they cannot recall already admitted requests, refund usage or undo tool effects. These restrictions govern Morons model-inference admission, not the network behavior of tools, local commands or web search. They are not a sandbox or a secrecy feature. See [ADR 0029](docs/adr/0029-provider-admission-and-data-use-policy.md).

### Managed IPython runtime

Release archives include a checksummed `morons-uv` helper. On the first `ipython` call, the companion uses it to prepare Morons-owned Python 3.11.15 with hash-locked `jupyter_client` 8.6.3 and `ipykernel` 6.30.1. Initial setup requires internet access to the reviewed Python and PyPI sources. The versioned runtime and download cache live under `~/.morons/python` on macOS/Linux or `%LOCALAPPDATA%\\morons.dev\\python` on Windows; after setup succeeds, ordinary reuse of that validated runtime does not require network access. Interrupted, stale, or invalid staging state is rebuilt under a process lock and never becomes the active runtime.

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
- `/settings`: choose the subagent model; `t` toggles Block training use and `r` toggles Require zero data retention
- `/login`: choose OpenCode hidden API-key input or experimental ChatGPT browser login (`Ctrl+K` shortcut)
- `/logout`: choose a provider for explicit, confirmed local credential removal
- `/context`: inspect context/cache/timing observations and the last accepted run's project-guidance paths and warnings; use arrows, PageUp/PageDown, Home/End to scroll
- `/compact [instructions]`: manually summarize an eligible old context prefix
- `r` in the session browser: rename the selected durable session
- `a` in the session browser: archive or unarchive the selected session
- `d` in the session browser: delete an archived session's Morons-owned history and attachments after confirmation; the working directory is never changed
- `Tab` / `Shift+Tab`: complete or navigate visible skill matches
- `Ctrl+V` (`Alt+V` on Windows): paste an image when available
- `Ctrl+X`: cancel the selected session's active run or command
- `Esc`: return to the session browser without cancelling server-owned work
- `?` in the session browser or `/help` in the composer: show usage and the security disclosure; question marks in text input remain literal
- `Ctrl+S`: stop the companion server and interrupt active work

Commands are noninteractive: standard input is closed, no PTY is provided, and output and runtime are bounded. `bash` and `ipython` still have your ordinary filesystem, environment, network, Git, credential-helper, and agent access.

`read` uses one-based line offsets and at most 200 lines per call. `write` requires an existing parent directory; create needed directories with `bash` first. Read-only preflight failures do not mutate the target, while failures after a possible mutation remain uncertain and are not automatically retried.

## Sessions and context

Each session is durably bound to one absolute working directory. Switching sessions or closing the client does not cancel server-owned work. Multiple sessions may use the same directory, so their filesystem effects can race even though their histories are independent.

The most recently selected or used reviewed model is the global default across sessions and client restarts. Opening an older session does not restore that session's historical model. If the saved default is unavailable, Morons uses another currently available reviewed model and reports the fallback. Each service/model pair pins its reviewed wire protocol. The built-in snapshots cover all 35 Go identifiers and all 66 Zen identifiers exposed by their public catalogs at review time across bounded Responses, Chat Completions, Anthropic Messages, and Gemini adapters; live catalogs may only mark reviewed entries available or unavailable. Training-eligible, non-ZDR, and privacy-undocumented models remain visible with explicit policy disclosures rather than being silently omitted or assigned a favorable classification.

Canonical transcript history remains durable. The terminal opens at the newest bounded window and pages older or newer history on demand, rendering only visible transcript blocks during steady-state frames. Automatic and manual compaction create source-bound lossy summaries for provider context without deleting canonical messages or image attachments. Compaction checks token, entry-count, and image budgets before dispatch, keeps recent complete turns when they fit, and leaves `/compact` available when old history fills ordinary context. Oversized old prefixes are summarized from explicitly bounded excerpts; summaries can lose detail. If the current run alone cannot fit, it fails durably rather than dropping current information or leaving the session busy. `!!` content is never included in provider context or summaries.

Background compaction is **on by default**. It can make additional inference requests using the successful run's exact service/model and billing identity; unused or discarded summaries can still consume quota or money. To opt out, stop the running server through its matching authenticated client, then launch with `MORONS_BACKGROUND_COMPACTION=0 morons`. An unset value or exactly `1` enables it; empty/invalid values disable it. The server captures this setting at startup, not from later clients. No other application's login or credentials are imported.

When enabled, one supervised background request globally can prepare a bounded older-prefix summary after a successful run. New input acceptance does not wait for that provider request. A ready result is not an active checkpoint: Morons installs it only before a later run's first provider request, after checking source, parent, model, credentials, guidance and every tail limit. Later arrivals wait for another run; stale or unhelpful results are discarded. Manual `/compact` drains background work and preserves its new guidance. Hard context jumps may still pause or fail. SQLite preparation uses the shared storage worker, and existing credential-dispatch coordination can delay foreground inference while a request is establishing its response. This is not a guarantee of lower bills or zero pauses. `/context` shows a bounded, query-time maintenance observation separately from foreground usage; reopen it to refresh. See [ADR 0026](docs/adr/0026-background-compaction-maintenance.md).

Context estimates reuse compatible successful provider usage plus a bounded new tail when available; conservative byte/item/image guards still apply independently. `/context` shows the estimate's source and the latest matching successful root call's cache and timing counters. Those counters exclude compaction, subagents, and failed requests and are not a complete bill. Foreground compaction counts and elapsed times are reported separately from the latest background job.

Every OpenCode Zen and Go inference request carries one stable, derived `x-opencode-session` identifier for its Morons conversation. The root value remains constant across the durable session's runs, foreground compaction, and tool turns. Each task child receives a distinct value stable across its own turns. Each background compaction request has a separate job conversation identity. These identifiers are not sent on public model-catalog requests.

## Project guidance and coding defaults

Morons combines a small shared coding core, parent/child role instructions, tool-specific guidance, and separately labeled project context. The defaults favor understanding the code, simple focused changes, reuse before new machinery, root-cause fixes, and honest verification—not code golf or removal of necessary safeguards/tests. Avoid redundant comments; new explanatory comments should fit on one line unless you request more. Required notices and documentation remain intact. Explicit user preferences override coding/workflow defaults, not harness constraints.

Before each newly accepted input, Morons checks `~/.morons` for global guidance, then ancestor directories from filesystem root through the selected directory. In each directory, the first present `AGENTS.override.md`, `AGENTS.md`, `AGENTS.MD`, `CLAUDE.md`, or `CLAUDE.MD` wins. An override shadows only its own directory's alternatives. Descendants are not recursively scanned; deeper guidance can be read explicitly. No `SYSTEM.md` replacement, script execution, reference following, or Pi/Codex configuration import occurs.

Guidance is **sent to the selected model service** as untrusted context. Do not put secrets in these files. Discovery skips final-component links, special files, invalid UTF-8 and oversized files with warnings rather than silently truncating instructions or falling back past an invalid preferred file. Limits are 64 ancestors, 16 files, 16 KiB/file, 32 KiB total content, 16 warnings and 64 KiB serialized context; discovery has bounded job admission and a cooperative five-second deadline. Ordinary filesystem syscalls can still block in the OS.

Files, warnings and enabled state are pinned in SQLite with each tool-enabled run. Later turns and task children receive that same guidance, not live rereads. New inputs refresh it; exact retries and recovery do not. `/context` shows the **last accepted run's** paths and warnings without reading source files or exposing their contents over IPC. Compaction does not summarize project guidance. Deleting a session never changes guidance files.

To disable automatic discovery, set `MORONS_NO_PROJECT_CONTEXT` (any value) before the companion starts; unset it to enable. Stop an existing companion with `Ctrl+S` before changing this server environment setting. This disables automatic loading, not ordinary tool access or user-supplied instructions. See [ADR 0025](docs/adr/0025-project-guidance-and-prompt-led-delegation.md).

## Subagents

By default the main selected model inspects and plans implementation work, uses one implementation child for a small change, then reviews changed code and runs relevant checks. Only independent assignments should run in parallel; dependent implementation and verification should not race. After a child failure, the prompt asks the parent to report partial progress and stop rather than retry or take over without explicit user direction. Children are told their existing budget of eight provider responses (including a final report), 24 tool calls and eight mutations. Discussion-only requests can be answered directly. This is **prompt-led delegation**, not enforced planner-only mode: the main agent retains its normal tools and can follow explicit requests for direct execution. Model compliance is not guaranteed by a prompt. See [ADR 0027](docs/adr/0027-tool-feedback-and-provider-output-validation.md).

The `task` tool follows a bounded OMP-style batch contract: the parent supplies shared context once and one to three self-contained assignments. By default children inherit the parent's model and credential identity. Before dispatch, each batch durably binds its selected provider/model and provider-local credential generation; cross-provider children never borrow an unrelated parent's counter. `/settings` can instead pin one exact available reviewed service/model pair for later task calls, including a different family, service, or wire protocol such as Zen GPT 5.6 Sol with Go GLM-5.3-Flash. Morons never silently substitutes another child model; each completed report discloses the selected model and protocol revision. Children run concurrently, receive the parent's pinned project guidance and only `read`, `write`, `edit`, `bash`, and `web_search` tools, and return input-ordered bounded reports. Active parent skills are not automatically inherited; include relevant task-specific context explicitly. They do not inherit the parent transcript, share IPython memory, recurse, continue in the background, or receive isolated worktrees. Children share the real selected directory, so parallel mutations can race.

## Skills

Morons reads standard `SKILL.md` directories from bundled, user, and project roots. Exact standalone `@name` tokens activate installed skills. Skills and their resources are untrusted instructions with the same tool authority as any other repository content.

## Platforms

The intended package targets are x86_64 and aarch64 on macOS, Linux, and Windows. CI runs natively on Linux x86_64/aarch64, macOS aarch64, and Windows x86_64/aarch64, and cross-checks the Intel macOS build. The `x86_64-apple-darwin` archive must also pass the native checklist on reviewed Intel hardware before Morons claims release support for it; cross-compilation is not that qualification. See [ADR 0007](docs/adr/0007-supported-processor-architectures.md).

## License

Apache-2.0. See [LICENSE](LICENSE).
