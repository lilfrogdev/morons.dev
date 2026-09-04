# ADR 0016: Managed IPython runtime

## Status

Accepted

## Context

ADR 0012 introduced one temporary persistent IPython kernel per active session, but required the selected system Python to already contain `jupyter_client` and `ipykernel`. That made a core tool fail on a clean installation and moved version compatibility onto every user.

The runtime is executable code with the local user's authority. Automatic preparation therefore must not execute installer shell pipelines, discover repository configuration, trust mutable package versions, write into system Python, expose Morons-managed provider credentials, or leave a partially prepared environment active. Setup also remains subject to cancellation and time bounds.

## Decision

Morons release archives contain three same-target executables: the user-facing `morons` client and internal `morons-server` and `morons-uv` companions. `morons-uv` is the unmodified Astral `uv` 0.12.9 executable. The packaging script downloads only the reviewed target asset from the fixed HTTPS GitHub release URL, verifies the target-specific SHA-256 digest committed in `scripts/fetch-uv.sh`, verifies its executable format, and records its packaged digest and version in `MANIFEST.txt`. The uv Apache-2.0 and MIT license texts ship in each archive.

On the first `ipython` operation, the server validates that its sibling `morons-uv` is the exact reviewed binary for the running OS and architecture. It then prepares this fixed runtime:

- CPython 3.11.15 from uv's version-pinned managed-Python catalog;
- `jupyter_client` 8.6.3;
- `ipykernel` 6.30.1; and
- all transitive Python packages pinned by `crates/morons-server/runtime/ipython-requirements.txt`.

The requirements lock is generated universally for Python 3.11 and contains an allowed SHA-256 set for every package artifact. Installation uses `--require-hashes`, binary distributions only, no project or user uv configuration, and the fixed `https://pypi.org/simple` index. uv's pinned managed-Python catalog supplies the interpreter download checksum. Morons never executes `curl | sh`, `pip`, or a repository-controlled bootstrap file.

Managed state is stored under the existing owner-controlled Morons application root: `~/.morons/python` on Unix and `%LOCALAPPDATA%\\morons.dev\\python` on Windows. It is separate from SQLite, attachments, credentials, system Python, user virtual environments, and selected repositories. A dedicated cache permits later rebuilds from already verified downloads.

Preparation has a ten-minute aggregate deadline, closed standard input, discarded diagnostics, process-tree ownership, exact cancellation, a 1 GiB managed-tree byte ceiling, and a 100,000-node ceiling checked before reuse and after each setup stage. A cross-process file lock serializes setup. On Unix, Morons builds in a fixed staging directory, validates exact import versions, writes a source-bound manifest, and atomically renames the environment to `runtime-v1`. Windows virtual-environment launchers embed their creation path, so Windows builds at the final versioned path under the exclusive lock and writes the exact manifest last as the validity marker. An interrupted or failed environment is never selected. A missing, stale, malformed, or failed runtime is removed and rebuilt under the lock. A valid runtime is checked once on first use after each server start and then cached in memory.

Managed uv and Python processes inherit the user's ordinary non-Python environment, including network and proxy configuration, while uv, pip, virtual-environment, `PYTHON*`, and project configuration inputs are removed or overridden. Managed kernels set isolated Python path/user-site behavior but otherwise retain the same trusted-local filesystem, process, network, Git, agent, and ordinary environment authority as `bash`. Morons-managed provider credentials remain server-owned and are never injected into setup or kernels.

`MORONS_PYTHON` remains an expert override. If it is nonempty when the server starts, managed setup is bypassed and that executable retains the previous environment behavior. This supports specialized Python distributions and offline installations without making manual setup the default.

Application protocol version 32 and persistence schema version 24 are unchanged.

## Consequences

- Packaged Morons provides a working persistent IPython runtime without requiring Python or manual `pip install` steps.
- First IPython use needs network access and may take longer while the interpreter and wheels are downloaded. Subsequent normal use and server restarts need no network.
- Release archives become larger because they include uv and its license texts.
- Release construction now depends on the fixed reviewed uv GitHub assets and fails closed on a checksum, format, layout, or target mismatch.
- Updating Python, uv, Jupyter packages, or the requirements lock requires a reviewed source change, new runtime version when compatibility changes, all six package builds, and release-candidate replay.
- Owner-only directories do not make runtime files confidential or immutable from arbitrary processes running as the same user. Runtime validation prevents accidental or malformed reuse; it is not a sandbox or same-user attestation boundary.
- Setup errors remain redacted and actionable. Users can retry with network access, reinstall a matching package, remove stopped Morons-managed Python state for a clean rebuild, or deliberately use `MORONS_PYTHON`.
