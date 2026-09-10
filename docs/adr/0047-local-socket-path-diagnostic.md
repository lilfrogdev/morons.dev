# ADR 0047: Redacted local socket path-length diagnostic

## Status

Accepted before implementation.

## Problem and reviewed evidence

A sufficiently long Unix HOME makes the fixed Morons runtime endpoint exceed the platform's Unix-domain socket pathname capacity. The retained macOS failure was the transport's `InvalidInput` length rejection. Normal companion startup discards server stderr, so a late bind rejection can instead leave the terminal with an unhelpful startup failure.

The pinned interprocess 2.4.3 `os/unix/ud_addr.rs` accepts pathname byte lengths **up to and including** `sun_path` capacity (104 on macOS, 108 on Linux), placing a terminator separately when necessary. Rust 1.98's safe standard socket-address constructor instead rejects equality. Substituting that constructor as-is would silently narrow admission by one byte. Error-message matching or relabeling every `InvalidInput` as length overflow would also be incorrect.

## Decision and boundary

Add an early, side-effect-free length check for the fixed generated runtime endpoint on reviewed macOS/Linux targets, with a closed typed local-control error for a proven overflow. Count native pathname bytes, including the actual runtime directory, separator and the unchanged Host-Epoch-derived endpoint identifier. Preserve inclusive 104/108-byte admission; these are transport capacities, not HOME character limits. Keep identifier construction in one place. No unsafe code or new dependencies are needed for these reviewed platform capacities. Other platforms retain their existing transport behavior; Windows named pipes do not acquire Unix length limits.

The check must run before client discovery can classify a too-long location as startable and before server preparation creates, loads, cleans or publishes control state. Rejecting geometry grants no authority. Existing HOME ownership/type checks and valid-length state/authentication checks remain required; malformed or insecure state is never replaced. NUL-containing input must not acquire the length-specific diagnosis merely because it is also long; unrelated I/O and validation errors keep their existing classifications. Do not inspect arbitrary error strings.

Expose the new category through a fixed, terminal-safe, actionable CLI description. It must identify the local socket pathname limit and explain that choosing a shorter absolute HOME selects separate Morons state rather than moving existing state. Do not include actual paths, Host Epochs, key bytes, remote strings or source error text. The direct server may retain the closed typed error classification; the ordinary CLI must not reduce this specific error to its generic control-state message or wait for a companion timeout.

This changes rejection timing and diagnostics, not transport routing or authority. No automatic shorter path, symlink alias, alternate socket namespace, relocation/migration, state cleanup, credential copy, endpoint/key replacement, retry, new configuration knob, deadline relaxation, IPC/schema revision or provider behavior is introduced. No claim is made that changing HOME preserves sessions, credentials or development configuration. Existing retained QA homes are not migrated or used as destructive test fixtures.

## Validation

- Native byte-length boundaries just below, at, and above capacity; multibyte/non-UTF-8 Unix paths; NUL not mislabeled.
- Client and server reject an overlong fresh synthetic location without creating state or making it startable. Retained synthetic sentinel contents remain unchanged on rejection.
- Valid-length missing/incomplete/starting/registered state, authentication, corruption and stale-endpoint behavior remain intact. If longer descriptive test labels now fail the earlier check, shorten only synthetic fixture labels while retaining collision resistance and every behavioral assertion.
- Fixed CLI Display/Debug behavior remains redacted and actionable for this category; unrelated control errors remain generic.
- Native before/after regression and targeted protocol/CLI tests, then locked formatting/check/Clippy/workspace tests/dependency gates before a local signed commit. Platform CI is separate and is not claimed without publishing the exact head under separate approval.

Morons remains a trusted-local harness, not a sandbox or rollback boundary. Synthetic tests and this maintenance task do not authorize a retained-state migration, paid request replay, push, tag or release.
