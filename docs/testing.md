# Testing Morons

Use Rust unit tests beside the module they verify. Larger suites use a module-local `tests/` directory, grouped by behavior, with shared fixtures in the parent test module or a small helper module. This preserves private-module access without exporting implementation details only for tests.

- Provider adapters: request/stream contracts and malformed-input tests beside each adapter.
- Persistence: separate migrations, session lifecycle, integrity, subscriptions, and run admission/history tests.
- Run supervisor: separate lifecycle, selection, context, compaction, maintenance, tools, and subagents; shared loopback provider fixtures in `tests/providers.rs`.
- Terminal application: separate input, transcript, presentation, session, model, and credential tests.
- Crate-root `tests/`: public-API/process integration, such as authenticated IPC and companion lifecycle.

Do not consolidate unrelated crates into one global test folder. Consolidate repeated setup, not distinct failure modes. Prefer typed/parsed assertions for protocol bodies; retain focused byte-level tests where exact encoding or terminal safety is the contract.

## Gates

```sh
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo deny check
cargo test --workspace --locked -- --test-threads 2
```

Two test threads match CI and reduce SQLite-heavy fixture contention. Timing probes must not impose wall-clock assertions on CI. Native-platform qualification is separate from cross-compilation.

Ignored live provider tests intentionally require non-echoing credential input and billable inference; Python gates require an installed or downloaded Jupyter runtime. Never run every ignored test indiscriminately. Follow `docs/release-candidate-qa.md` and obtain authorization before billable requests or credential-state changes.

## OpenAI OAuth core

```sh
cargo test -p morons-server --lib --locked openai_auth -- --test-threads 2
```

These tests use ephemeral loopback ports, disposable storage and synthetic tokens. They do not bind the production callback port, contact OpenAI, read real credential files or migrate retained state. They cover fixed PKCE/form fields, hostile callbacks, cancellation/drop/deadlines, token envelopes/claims, redaction, bounded HTTP and no retry/redirect following. Custody tests cover provider-scoped identity/idempotency, private files/checksums, poisoning after failed writes, same-account single-flight refresh, cancellation/abandonment, restart without replay, secret exclusion from SQLite and schema-27 migration preserving OpenCode bytes. They do not establish JWT-signature verification, real token rotation, live login/inference or native-release qualification.

Login controls use IPC 40 and SQLite 28. Do not launch its binaries against retained QA state without a separately approved migration plan; deterministic fixtures do not authorize changing existing diagnostics. [ADR 0019](adr/0019-openai-subscription-authentication-and-hosted-search.md) still requires the reviewed coding adapter and policy bindings before owner live testing.

Synthetic login-control tests additionally cover connection-scoped cancellation, disconnects, slow consumers, abandoned drains, shutdown during admission, committed installation/cancellation races, status and provider-local removal, closed/redacted framing diagnostics, client outcome scope/generation validation with no reconnect/replay, and terminal-safe scrollable URL dialogs that reject paste/image input and clear URLs on cancellation. The old OpenCode hidden-input/removal tests now choose OpenCode in the provider menu; their secrecy and generation assertions remain. No test opens a browser or the production callback port.

## Native Codex adapter

```sh
cargo test -p morons-server --lib --locked openai_codex -- --test-threads 2
```

[ADR 0028](adr/0028-native-codex-responses-contract.md) pins the full-Responses GPT-5.5 contract. Tests use only synthetic credentials and loopback HTTP: fixed body/headers, strict tools and normalized images, local-only output limits, policy checks, provider-instance/turn/generation/request-sequence binding, receipt-bound reasoning and sticky-header isolation, lease release after headers, cancellation/drop/deadlines, contradictory/oversized metadata and no retry/fallback. The adapter is not yet reachable by application inference. These checks do not qualify subscription entitlement, current account policy, remote generation/spend limits or live interoperability. Global durable settings and root/child/maintenance binding remain admission gates; no retained QA upgrade or real browser/provider call is implied.

## Local performance probes

Run only these explicitly named, non-network probes (not the billable ignored tests):

```sh
cargo test -p morons-server --lib --release --locked measure_ -- --ignored --nocapture --test-threads 1
```

On macOS ARM64 / Rust 1.98.0, medians of three consecutive warm runs were:

| Probe | Fresh/full path | Reused/metadata path |
|---|---:|---:|
| 2,000 Responses requests with fixed root tools | 19.16 ms | 5.53 ms |
| 2,000 Gemini requests with fixed root tools | 28.83 ms | 4.44 ms |
| 200 observations with four 2×2 image attachments | 56.75 ms full context loads | 19.31 ms metadata status |

These compare paths in the same release build, not whole-application before/after latency. They exclude real provider latency/cost; fixtures use temporary state and fake credentials. No timing ratio is asserted in CI. Large histories, cold disks, other architectures and provider caching need separate qualification.

The [background-compaction procedure](background-compaction-qa.md) adds a non-network maintenance timing probe and separates deterministic lifecycle evidence from explicitly authorized live inference. Background compaction defaults on; use `MORONS_BACKGROUND_COMPACTION=0` for controlled comparison or to opt out. Default selection is not a substitute for live or release qualification.

Retain migration tests while their source schemas remain supported, including obsolete workspace-era fixtures: these protect safe upgrades and non-interference with selected directories. Name migration tests for their actual target (`..._migrates_to_current_version`) rather than leaving a historical destination in the name.
