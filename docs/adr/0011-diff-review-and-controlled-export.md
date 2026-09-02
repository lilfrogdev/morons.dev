# ADR 0011: Diff review and controlled export

## Status

Superseded by ADR 0012

## Context

A session now has an immutable imported baseline and one authoritative active worktree generation. Structured tools and sandboxed commands can change only that active generation, but the local owner needs to inspect those changes and deliberately copy them out of Morons.

The baseline, active worktree, generated files, paths, output destination, and concurrent filesystem state are untrusted. Review must not invoke Git or expose host paths. Export is an external filesystem effect that cannot commit atomically with SQLite and must not become an implicit write-back to the imported source.

## Decision

### Review boundary

The authenticated local owner may request a bounded read-only diff for a ready, idle, unblocked session. The server resolves the immutable baseline and exact committed active generation while holding the session workspace lease. Clients cannot select a baseline, generation, host path, comparison program, ignore rule, or diff implementation.

Review traverses both trees in raw UTF-8 byte order and admits only the same ordinary bounded path and node subset accepted by workspace validation. Links, reparse points, alternate streams, special files, changed identities, collisions, and resource exhaustion fail closed.

A review entry classifies one path as added, modified, deleted, or executable-mode changed. It includes bounded architecture-neutral sizes and SHA-256 content digests. Bounded UTF-8 files may include a server-generated plain unified excerpt; binary, oversized, or invalid UTF-8 files receive metadata only. Review output contains repository-relative paths and sanitized content, never baseline, generation, workspace, source, or host-absolute paths.

Pages use a strict opaque cursor binding the session, active generation, review-format version, and last emitted relative path. A successor generation invalidates the cursor. Review is unavailable while a run or workspace operation can change the session.

### Export boundary

Export is a separate authenticated local-owner mutation. It requires a stable mutation identity, the session, the exact reviewed active-generation identity carried by a review cursor, and one bounded normalized absolute UTF-8 destination path. The destination path is transient input, is redacted from debug output, and is represented durably only by a role-separated SHA-256 digest.

The destination must not exist and must not overlap Morons application, control, runtime, data, backup, credential, image, operation, or workspace roots. Its existing parent chain must be ordinary and free of links or reparse points. Repository import durably records a role-separated digest of the identity-checked canonical source root, without retaining path bytes. Export computes the same role-separated digest for every identity-checked destination ancestor and rejects the destination if any ancestor is the imported source root. Morons never accepts overwrite, merge, conflict resolution, source write-back, Git metadata, ownership, ACL, extended-attribute, alternate-stream, sparse-file, or special-permission options.

Under the session workspace lease, trusted server code revalidates the reviewed generation and copies it into an operation-bound private staging directory beneath Morons' protected export-operation root. It uses the repository import path and resource policy, strips untrusted auxiliary metadata, synchronizes the complete private snapshot, and verifies the reviewed canonical manifest. Only then does it commit dispatch. After dispatch, it opens and pins the validated destination parent chain, copies the private snapshot handle-relatively into an operation-named sibling staging directory, synchronizes and revalidates that complete tree, and atomically renames it without replacement to the absent destination. The parent directory is synchronized where the platform supports it. Export never executes repository code, Git, hooks, or a provider request.

A completed export returns only counts and logical bytes. It never returns or records the destination path. The TUI requires explicit confirmation that export creates one new independent tree and never merges into an existing repository.

### Durability and recovery

Export records prepared, dispatched, completed, not-applied, or uncertain facts plus idempotency and structured audit facts. Prepared state binds the session, active generation, generation manifest, canonical source-root digest, destination-path digest, operation identity, format, and limits without storing either path.

Startup performs no external destination lookup because the destination path is not durable. Prepared but undispatched export state becomes not applied and its exact private staging is removed. No external sibling staging can exist before dispatch. Dispatched state without a committed outcome becomes uncertain; its private snapshot is retained, and no private snapshot, external sibling, or destination is retried, completed, inspected, or removed automatically. It does not block workspace mutation because export cannot change authoritative session state, but its audit history remains uncertain.

An exact deliberate live retry must use the same transient destination path and request identity. It first verifies the destination-path digest and source-root exclusion, then may inspect only its exact private snapshot, operation-named sibling staging, and final destination through pinned no-follow handles. If the destination already exists, the retry completes only when its full reviewed manifest proves the exact export. If the destination is absent and an exact complete sibling or private snapshot remains, the retry may deliberately finish the no-replace publication. Any missing, changed, ambiguous, or conflicting state preserves the uncertain result. A new export requires a new absent destination and mutation identity.

### Limits and presentation

Review and export enforce fixed limits for paths, depth, entries, file bytes, total bytes, diff text, lines, page entries, elapsed time, and concurrent workspace operations. Every durable and protocol value uses checked fixed-width conversion.

Ratatui renders review text only through terminal-safe bounded cells. It provides no editor, terminal, raw filesystem browser, arbitrary destination merge, or automatic apply operation.

### Validation

Implementation requires tests for added, modified, deleted, executable, binary, invalid-UTF-8, empty, and oversized files; stable pagination and stale cursors; links, reparse points, special files, collisions, changing trees, and bounds; idle-session authorization; destination absence and protected-root overlap; private staging, synchronization, atomic publication, exact retry, and every crash seam; no source, baseline, metadata, credential, or host-path disclosure; no original-source write; and native macOS, Linux, Windows x64, and Windows ARM64 behavior.

## Consequences

- The local owner can review all authoritative workspace changes without Git or host-path access.
- Export creates a deliberate independent tree and never silently writes back or merges.
- Destination absence makes publication simple and fail-closed but requires the owner to choose a new path for each export.
- A crash after dispatch can leave an uncertain external export that Morons will not retry or erase automatically.
- Diff excerpts are intentionally bounded and are not an IDE or full patch engine.
- New repository imports retain only a canonical source-root digest so export can reject descendants of the original source without retaining or disclosing the source path.
