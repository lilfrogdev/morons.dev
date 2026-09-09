# ADR 0044: Explicit task-binding deletion

## Status and finding

Accepted before implementation. OpenAI-web lifecycle tests exposed a pre-existing SQLite30 task-binding `ON DELETE CASCADE` incompatible with the storage worker's deliberate `SQLITE_LIMIT_TRIGGER_DEPTH=0`. Deleting a session with tool-call history can invoke that trigger path even if no task binding exists. This is independent of hosted search and receives a focused repair rather than a relaxed SQLite policy.

## Decision

SQLite31 rebuilds only `task_model_bindings` without the cascade, retaining column order, every row, source digest, identity, index and the original provider-binding epoch. Explicitly delete session-owned task binding rows before their owning tool calls and accepted runs in the existing storage-worker deletion transaction. Do not enable triggers, raise recursion limits, alter canonical input/results, manufacture old bindings or touch the selected working directory. Migration backup, integrity validation and fail-closed recovery remain unchanged. No IPC, credentials, provider routes, inference retries or selected model changes.

## Qualification

First reproduce failure on ordinary completed tool history and on a bound task. Test deletion of both, preserved selected files, populated schema30 migration and unchanged binding bytes. Run all locked gates and exact-head platform/security checks. This does not authorize a retained QA-home migration or release.
