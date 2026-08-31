# ADR 0007: Supported processor architectures

## Status

Accepted

## Context

Morons is intended to run on contemporary Intel/AMD and Arm computers across macOS, Linux, and Windows. Local IPC, owner-only filesystem controls, SQLite durability, server startup, terminal handling, TLS, and future sandboxing all contain operating-system-specific behavior that cannot be proven by compiling for one processor architecture.

Rust target support and a successful cross-compilation do not prove that peer credentials, named-pipe security, filesystem synchronization, terminal restoration, process startup, or bundled native dependencies work on a real target. At the same time, processor architecture must not become an application identity, authorization input, protocol field, or persistence-layout dependency.

## Decision

### Supported target set

The first release-supported processor architectures are 64-bit `x86_64` and 64-bit `aarch64`. The intended operating-system target set is:

- `x86_64-apple-darwin`;
- `aarch64-apple-darwin`;
- `x86_64-unknown-linux-gnu`;
- `aarch64-unknown-linux-gnu`;
- `x86_64-pc-windows-msvc`; and
- `aarch64-pc-windows-msvc`.

In product language, Intel/AMD support means `x86_64` and Arm support means `aarch64`. Thirty-two-bit x86, Armv7, Linux musl, Android, iOS, and other targets are not release-supported by this decision.

A target is described as release-supported only after the complete native security and behavior suite passes on that target. Source compatibility or cross-compilation alone is reported as such and is not represented as native support.

### Architecture-neutral contracts

Application identifiers, protocol records, authentication inputs, durable payloads, cursor encodings, request fingerprints, and provider wire values use explicit sizes and byte order. They must not serialize `usize`, pointer values, native enum representations, native path encodings, process handles, struct memory, alignment padding, or host-endian integers.

SQLite records use checked fixed-width domain conversions. Every conversion between a collection length, filesystem size, SQLite integer, protocol integer, and `usize` must reject overflow or truncation. Resource limits and security policy are the same on `x86_64` and `aarch64` unless an accepted architecture decision documents a stricter target-specific limit.

Processor architecture is diagnostic metadata only. It is never authentication evidence, authorization evidence, a capability, a session property, or a reason to accept a different protocol or persistence schema.

A client and server with the same supported application and authentication protocol versions may communicate when the operating system can run them even if one process is translated or uses a different processor architecture. Authentication, operating-system peer checks, and server authority remain unchanged.

### Implementation posture

Platform-specific code is selected by operating system rather than processor architecture unless the operating-system API genuinely differs by architecture. Architecture-specific branches require a concrete reviewed need and native tests. No target may add workspace-owned `unsafe` code merely to reproduce functionality available from Rust, the operating system, or an existing reviewed dependency.

Every direct dependency, bundled native library, terminal backend, and future sandbox component must support both architecture families on each applicable operating system. A dependency that silently downloads target binaries at build or runtime is not admitted. Native build scripts, prebuilt objects, assembly implementations, and target-feature detection receive the same source, license, pinning, and CI review as Rust code.

Cryptographic and provider behavior must remain valid when optional processor acceleration is unavailable. Runtime feature detection must fall back safely and must not change protocol output or authorization behavior.

### Distribution

Release artifacts are produced separately for each supported Rust target triple. Each artifact contains matching `morons` and `morons-server` executables from the same source revision and version. Universal macOS binaries and one archive containing several target executables are not required.

The client locates its packaged server companion by exact installation-relative path as defined by ADR 0006. It does not select or download an executable based on untrusted registration, repository, model, or configuration data. Packaging verifies the target triple and hashes of both executables before publication.

Persistent control, credential, database, backup, and workspace formats are shared across supported architectures. Moving owner-controlled state between processors on the same supported operating system does not require a schema conversion, although normal schema-version and filesystem-security validation still applies.

### Validation

Pull-request and `main` CI run the full formatting, compilation, Clippy, deterministic test, authenticated IPC, SQLite, filesystem, provider-fixture, and terminal-state suites natively on standard hosted `x86_64` and `aarch64` runners where those runners are available to the repository.

When a native hosted runner is unavailable, every pull request still performs a locked cross-target compilation for that target. A separate native release gate on reviewed hardware must pass before publishing that target. Cross-target compilation never substitutes for native testing of IPC peer identity, DACLs, Unix ownership and modes, filesystem synchronization, process lifecycle, terminal behavior, cancellation, or recovery.

CI logs the exact Rust host and target, operating-system version, and processor architecture so a moving runner label cannot silently change coverage. Runner labels and action commits remain explicit and reviewed. Emulation may supplement testing but is not the only release gate for a target.

Architecture regression tests cover:

- fixed protocol and authentication byte shapes;
- identifier, cursor, integer, length, and timestamp bounds;
- SQLite creation, migration, backup, and projection rebuild;
- authenticated Unix sockets or Windows named pipes and peer identity;
- owner-only filesystem controls and synchronization;
- client companion discovery, concurrent startup, and graceful shutdown;
- TLS/provider fixtures, cancellation, and deadlines; and
- Ratatui input, rendering, and terminal restoration.

## Consequences

- Intel/AMD and Arm are deliberate product targets instead of incidental compiler outcomes.
- Protocol and durable state remain portable across processor architectures.
- CI and release validation cost increases because security-sensitive behavior must run natively rather than relying only on cross-compilation.
- Separate target artifacts keep packaging and companion-process discovery explicit.
- Thirty-two-bit and additional libc or mobile targets require a later architecture decision and their own resource, dependency, packaging, and security validation.
