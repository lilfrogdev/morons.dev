# Opt-in daily debug files

This extends ADR0049/0050's sink and activation contract, not its closed event schema.
`morons --debug` launches the matching companion with `--debug`. It refuses an
already running server: connect normally to stop it with Ctrl+S when work is idle,
then start again. It never enables logging remotely or restarts a server.
Normal reconnects use `morons` and do not change the server's logging mode.
`morons --debug-status` queries the authenticated server without starting one and
prints JSON health (active, writer failure, daily cap, drops, and log directory).
This read-only protocol addition exposes no event content and cannot enable logging.
Format 2 records add Unix-millisecond write timestamps, process IDs, severity and
component labels; existing operation IDs and sanitized failure fields remain.

Explicit server debug mode writes only reviewed `MORONS_DEBUG` records to private
UTC daily files in the existing profile's `logs` directory (`~/.morons/logs` on
Unix, LocalAppData/morons.dev/logs on Windows). Ordinary stderr is not captured.
Files are capped at 8 MiB per day, with no automatic deletion or replay. Queue, record-size and shutdown bounds remain. Admission is limited to 512 records
per monotonic 60-second window, with bounded dropped-record summaries; the budget
renews throughout the server lifetime. Daily-cap drops resume on the next UTC day.
Logs are lossy observations, not durable evidence.
Root tool output/resource-limit failures emit warning-level `tool_limit` records
before result persistence, correlated by run and local call IDs. Records contain
only tool/error categories and, for Bash, retained stdout/stderr byte counts and
the per-stream byte cap; counts are not total bytes produced. A record observes
execution, not a successful database commit. Commands, paths and output content
are excluded. Provider and persistence limits retain their separate diagnostics.
New `context_limit` warnings identify selected persistence checks using fixed enums,
run/call identifiers, measured sizes and limits only. Coverage currently includes
tool payload encoding and the final run-context budget guard (tokens, source bytes,
reserved entries, image count and image bytes). Arithmetic and earlier compaction
failures are not all individually instrumented. These pre-commit observations do
not prove persistence or authorize retry. No payload content is recorded.

Root `provider_failure` warnings correlate session/run IDs with fixed stages
(compaction, request construction, dispatch preparation, response execution), the
original `ProviderError` category, and the uncertainty classification supplied to
persistence. They observe failure handling, not a successful commit. The shared
error type does not retain HTTP status codes; these events do not invent them or
capture raw transport errors. Logging remains opt-in, bounded and lossy, with no
changes to credentials, retries, timeouts, routing or failure semantics.

New files use exclusive creation; existing files and directories are validated,
with symlinks rejected. Owner-only permissions are not secrecy from same-user
commands. No credentials, raw payloads, prompts, queries or tool output are added.

Endpoint preparation precedes file logger activation so the server owns the
profile lock before opening logs. Endpoint preparation itself cannot be captured
by the file logger. File setup failures fail debug startup with a fixed message;
later writer failures disable diagnostics without stopping application work.

## CLI clipboard diagnostics

Selection-copy failures are not rendered as toasts or status messages. The CLI
lazily creates `morons-cli-clipboard-<pid>-<timestamp>.log` in the OS temporary
directory on the first failure or selection invalidation. Each process writes at
most 64 KiB, then stops logging; files are not automatically deleted. Logging is
best-effort and an unavailable log never triggers a clipboard retry. Unix files
are created with mode 0600; on Windows they inherit temporary-directory access.
These files contain only timestamps and fixed failure categories, never selected
text, login URLs, credentials, or raw backend errors. They are separate from the
opt-in server debug log and do not require restarting the server. Successful
selection copies display only a compact green `Copied` toast. Login-dialog
recovery messages are unchanged.
