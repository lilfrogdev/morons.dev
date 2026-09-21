# ADR 0055: Bash without implicit execution deadlines

## Status

Accepted. Supersedes the shell execution deadlines in ADR 0012.

## Decision

Agent Bash and `!`/`!!` command mode share an executor with no wall-clock or inactivity deadline. Like Pi's default Bash behavior, silent and long-running commands may finish normally instead of being killed by an implicit timer. The command-only tool API is unchanged; this does not introduce Pi's optional timeout parameter.

Cancellation, server shutdown supervision, output bounds, capture-error handling, process-tree termination, and bounded process-group cleanup waits remain in place. Provider, network, IPC, and IPython deadlines are unchanged. No failed or uncertain operation is automatically replayed.

## Consequences

A hung command can occupy its run indefinitely until explicitly cancelled or the server shuts down. This is a deliberate trusted-local lifecycle policy, not a sandbox guarantee. Silence is not evidence of failure; the owner remains responsible for stopping unwanted commands. Cancellation cannot undo completed effects. Running servers need a normal restart to adopt this change; editing the executor does not alter the current process.

## Verification

Retain cancellation and output-exhaustion tests. An opt-in slow regression runs silently beyond both former deadlines and must complete successfully; its test-only watchdog cancels after a generous bound. Run with `cargo test -p morons-server --lib bash_silent_command_outlives_former_deadlines -- --ignored`.
