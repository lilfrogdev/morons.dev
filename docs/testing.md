# Testing Morons

Use Rust unit tests beside the module they verify. Larger suites use a module-local `tests/` directory, grouped by behavior, with shared fixtures in the parent test module or a small helper module. This preserves private-module access without exporting implementation details only for tests.

- Provider adapters: request/stream contracts and malformed-input tests beside each adapter.
- Persistence: separate migrations, session lifecycle, integrity, subscriptions, and run admission/history tests.
- Run supervisor: separate lifecycle, selection, context, compaction, tools, and subagents; shared loopback provider fixtures in `tests/providers.rs`.
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

Retain migration tests while their source schemas remain supported, including obsolete workspace-era fixtures: these protect safe upgrades and non-interference with selected directories. Name migration tests for their actual target (`..._migrates_to_current_version`) rather than leaving a historical destination in the name.
