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

Packaged use does not require Rust or a preinstalled Python. Bash is required for the `bash` tool and `!`/`!!` command modes; on Windows, Morons discovers a normal Git for Windows installation or uses the expert `MORONS_BASH` override set before the companion starts. Model inference requires the selected provider's credential: an OpenCode Zen/Go API key or native ChatGPT login. Neither is required for launch or local session management. First managed-IPython setup and provider operations require network access. `web_search` requires ChatGPT login, even when the coding model uses OpenCode. It uses only OpenAI-hosted search; there is no Brave adapter or fallback. See the separate search identity and usage below.

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

To update, use the **old matching client** to stop the running companion with `Ctrl+S`, verify and extract the new archive into a new complete installation directory, and launch that directory's `morons`. Do not copy individual executables over an old package or mix companions from different versions. Current source uses **IPC45 / SQLite33**; an IPC44 client cannot control an IPC45 server. Durable state migrates forward on the next start. The schema32→33 migration records an accounting epoch without rewriting old runs and creates a database-only schema32 backup. That backup does not include credentials, IPC keys, attachment files or selected working directories; it is not a complete session/project backup. Downgrading migrated state is unsupported. See [the upgrade and test guidance](docs/testing.md#current-state-and-upgrades).

On every client launch, read and acknowledge the trusted-local authority notice. `/login` or `Ctrl+K` chooses OpenCode or ChatGPT. OpenCode API-key input remains non-echoing. After explicit confirmation, ChatGPT login automatically opens your default browser. If needed, click **Open browser** (`o`/Enter) or **Copy link** (`c`) to copy the complete URL for pasting into your browser; terminal-dependent text selection is not required. Clipboard managers may retain copied links. Never paste callback codes or tokens into Morons. If the browser reports a rejected callback, share only its fixed reason—not the callback URL—and do not refresh/replay that callback. A code-received browser page is not login success: Morons must save the credential. If token validation fails, report only Morons' fixed rejection reason, never tokens or response bodies. Esc requests cancellation, but a save already started may complete. **ChatGPT models are explicitly selectable: GPT-6 Astra, GPT-5.6 Sol/Luna/Terra, Daybreak Blue, and GPT-5.5.** Daybreak Blue requires provider approval/provisioning; selection does not prove account access. The native decoder accepts the exact response name `gpt-5.6-sol` for a request selecting `gpt-daybreak-blue-latest`, while retaining Daybreak's requested safeguards identity and canonical attribution. Reverse, non-native and unreviewed mappings still reject without fallback or retry; see [ADR 0048](docs/adr/0048-native-daybreak-response-alias.md). This compatibility rule does not establish entitlement, billing equivalence or universal native interoperability. A native protocol failure can show a fixed rejection reason and local run locator while connected; share only that reason, not provider responses or credentials. This status is ephemeral and never implies retry permission. See [the reviewed model contract](docs/adr/0035-reviewed-native-model-expansion.md). Each output limit is local validation, not a remote generation or spending cap; remote work may already exceed it. Unknown account-controlled data policy cannot satisfy either restrictive setting. Login never changes the selected model. `/logout` chooses a provider for confirmed local removal; remote authorization and dispatched work may remain. Revoke access through the provider account when needed. Credentials live in dedicated owner-controlled state outside SQLite and are never intentionally exposed to tools or kernels. Maintainers follow [the release procedure](docs/releasing.md) and [release-candidate QA checklist](docs/release-candidate-qa.md).

### Data-use restrictions

Both `/settings` restrictions default **off** and are independent. They use reviewed manifest metadata, not account claims: unknown/account-controlled policy cannot satisfy a restriction. Blocked models remain visible and selected without fallback, but new selections, inputs and provider dispatches (including children and compaction) are checked by the server. Policy changes invalidate stale background summaries before dispatch/installation; they cannot recall already admitted requests, refund usage or undo tool effects. These restrictions govern Morons model-inference admission, including OpenAI-hosted web search, not arbitrary network access through Bash, Python or local commands. They are not a sandbox or a secrecy feature. See [ADR 0029](docs/adr/0029-provider-admission-and-data-use-policy.md).

### OpenAI web search

`web_search(query)` sends only a bounded query and fixed research instructions to a separate GPT-5.5 hosted-search request through ChatGPT. This is not a main-model selection or fallback: Astra/Daybreak coding and GLM-5.3-Flash children keep their own selected identities. The search model is fixed independently of them. Search can search, open and find within public pages on OpenAI's servers; it does not control your browser or attach the parent transcript, files or skills. Model-selected queries can still contain information the coding agent read, so query isolation is not a secrecy boundary.

Results show a bounded answer, literal source URLs and separate search token/action receipts. URLs and page content are untrusted; opening a source is an owner action, not automatic navigation. Search is model inference and both `/settings` data-use restrictions apply; unknown native policy cannot satisfy either restriction. Missing login fails without using another provider. A dispatched search with an unknown outcome stops its owning root tool or task as uncertain without replay; absence of a receipt is not evidence of zero usage. Completed child search receipts are separate from child coding usage; interrupted batches may not deliver child receipts. Local limits are not remote spending caps.

A rejected or incomplete search remains **Uncertain** and is not retried. New failures show only fixed execution/validation stages and error categories; no raw provider details or credentials are captured. Historical failures cannot acquire a more precise diagnosis retroactively. Consulted-source metadata has its own128-descriptor aggregate bound, separate from ten returned citations; generated queries and hosted actions retain their eight-entry bounds. These are local acceptance limits, not remote spending controls. See [ADR 0045](docs/adr/0045-hosted-web-failure-diagnostics.md) and [ADR 0046](docs/adr/0046-hosted-search-source-metadata-bounds.md).

OpenAI-only application integration is undergoing synthetic/platform and live qualification; do not interpret implementation as a completed live-search qualification. Historical Brave results are read-only compatibility, not an available backend.

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

Canonical transcript history remains durable. The terminal opens at the newest bounded window and pages older or newer history on demand, rendering only visible transcript blocks during steady-state frames.

Automatic and manual compaction create source-bound lossy summaries without deleting canonical messages or attachments. Old accepted runs retain legacy whole-current-run protection. New eligible native text roots use [execution policy1](docs/adr/0052-context-accounting-policy.md): compatible immediately preceding committed same-run usage may inform admission, and completed tool batches may be compacted while retaining the exact original user intent and latest batch. At most **four foreground compaction attempts per run**, including initial/manual compaction, are allowed with advancing source and intervening completed coding progress. Summaries are extra provider requests outside the **32 completed coding-turn** counter; neither counter is a spending cap. After checkpoint commit, ephemeral continuation is reset.

Missing, zero, uncertain or wrong-scope usage falls back conservatively. The native policy's 1 MiB source cap and existing entry/item, image, schema, continuation and encoding bounds remain independent of token estimates. OpenCode roots, children, images and background maintenance retain conservative accounting. If mandatory retained information still cannot fit, the run fails durably without hidden truncation or leaving the session busy. Failed/uncertain summaries stop without retry. Bounded summary excerpts can lose detail; `!!` content never enters provider context or summaries.

Background compaction is **on by default**. It can make additional inference requests using the successful run's exact service/model and billing identity; unused or discarded summaries can still consume quota or money. To opt out, stop the running server through its matching authenticated client, then launch with `MORONS_BACKGROUND_COMPACTION=0 morons`. An unset value or exactly `1` enables it; empty/invalid values disable it. The server captures this setting at startup, not from later clients. No other application's login or credentials are imported.

When enabled, one supervised background request globally can prepare a bounded older-prefix summary after a successful run. New input acceptance does not wait for that provider request. A ready result is not an active checkpoint: Morons installs it only before a later run's first provider request, after checking source, parent, model, credentials, guidance and every tail limit. Later arrivals wait for another run; stale or unhelpful results are discarded. Manual `/compact` drains background work and preserves its new guidance. Hard context jumps may still pause or fail. SQLite preparation uses the shared storage worker, and existing credential-dispatch coordination can delay foreground inference while a request is establishing its response. This is not a guarantee of lower bills or zero pauses. `/context` shows a bounded, query-time maintenance observation separately from foreground usage; reopen it to refresh. See [ADR 0026](docs/adr/0026-background-compaction-maintenance.md).

`/context` separates the admission policy/estimate, legacy byte-heavy estimate, source-byte envelope, and latest matching successful root call's cache/timing observations. Legacy usage is advisory; native admission requires the stricter same-run provenance above. Cached input is included once. Status is a changing prediction, not an exact tokenizer count, request-preparation receipt or complete bill. Root usage excludes compaction, subagents and failed requests; foreground compaction counts and elapsed times are separate from the latest background job.

For opt-in diagnostics, stop the existing companion with its matching client, run the exact `morons-server --debug` in a separate terminal under the same profile, then connect its matching client. Only this explicit foreground flag enables bounded closed metadata on stderr; `RUST_LOG` and debug builds do not enable it. There is no automatic log file or extra normal UI output. Records may be lost and missing receipts mean unknown usage, not zero. See [the debug procedure](docs/testing.md#opt-in-provider-debugging).

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
