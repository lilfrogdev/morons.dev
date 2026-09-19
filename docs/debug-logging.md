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
New files use exclusive creation; existing files and directories are validated,
with symlinks rejected. Owner-only permissions are not secrecy from same-user
commands. No credentials, raw payloads, prompts, queries or tool output are added.

Endpoint preparation precedes file logger activation so the server owns the
profile lock before opening logs. Endpoint preparation itself cannot be captured
by the file logger. File setup failures fail debug startup with a fixed message;
later writer failures disable diagnostics without stopping application work.
