# ADR 0024: Unpublished control-state discovery

## Status

Accepted

## Problem

A client may observe a legitimate first-start initializer after its private authentication-key file is created but before all bytes have been written. The server holds the stable host lock and has not published an endpoint. Reading the key at this point can fail on transient incomplete data instead of reporting the already-defined `Starting` state.

A macOS lifecycle CI helper failed without diagnostics because its stderr was discarded. The original failure's precise cause cannot be recovered from that log; the initialization window above is independently reproducible with a deterministic regression rather than assumed to be that failure's cause.

## Decision and boundary (before implementation)

After validating private root/control directories and the stable host-lock file, report `Starting` while the host lock is held and no endpoint registration exists. Do not inspect an in-progress key until publication. `Starting` grants no connection, authentication or application authority; clients wait only within their existing bounded startup deadline and cannot start a replacement server.

If a registration exists, or no initializer owns the lock, keep all existing key validation and fail-closed behavior. A published or abandoned malformed key remains an error. Do not rewrite, replace, repair, import or delete a key. No transport authentication, IPC wire, provider credential, selected-directory or persistence boundary changes.

Expose subprocess stderr in the isolated lifecycle tests so a future failure has diagnostics. These test processes have a cleared environment and fresh private HOME, not a user's credential state.

## Concurrent observation follow-up (2026-09-06)

A later macOS concurrent-client helper returned only the redacted Control classification. One hundred local process repetitions did not reproduce it, so its precise cause remains unproven. Independent deterministic interleavings expose two additional valid startup windows: a host lock can appear after an initial missing-file probe but before the directory is checked; an initializer can acquire the lock and begin writing its key after a client observed the lock as free.

Before implementing the repair, retain the same boundary: a newly appeared host lock goes through the ordinary private-file/lock validation, not a missing-lock corruption result. A failed key load may defer to Starting only after rechecking that an initializer currently owns the stable lock and no endpoint is published. This is bounded observation, not retry of an external effect or authorization from partial data. Published/abandoned corrupt keys and genuinely missing locks remain fatal; no key, endpoint, permissions, authentication or timeout is repaired/relaxed. Test observers reproduce the precise interleavings without sleeps. The process helper reports only fixed InvalidState reasons or I/O ErrorKind, never arbitrary JSON, paths or key bytes.

## Validation

Simulate a private truncated key while an unpublished server holds the lock: discovery must report `Starting`, and a competing server must still be rejected. Restore the key and publish: discovery becomes `Registered`. Truncation after publication and after releasing the lock must both fail closed. Run protocol tests, repeated native lifecycle tests, workspace gates and platform CI.
