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

## Terminal input delivery

[ADR 0034](adr/0034-lossless-terminal-input-delivery.md) covers bounded backpressure without silent input loss. `cargo test -p morons-cli --lib --locked terminal::queue_tests` verifies burst key/paste/Enter order with a full queue, closed-receiver rejection and Drop waking a backpressured reader before joining it. These synthetic tests use no real terminal, clipboard or inference. A live draft-only check must compare the complete intended prompt before Enter; preserve any already submitted truncated input instead of rewriting or replaying it. Input delivery success does not qualify provider protocol compatibility.

## OpenAI OAuth core

```sh
cargo test -p morons-server --lib --locked openai_auth -- --test-threads 2
```

These tests use ephemeral loopback ports, disposable storage and synthetic tokens. They do not bind the production callback port, contact OpenAI, read real credential files or migrate retained state. They cover fixed PKCE/form fields, hostile callbacks, cancellation/drop/deadlines, token envelopes/claims, redaction, bounded HTTP and no retry/redirect following. Custody tests cover provider-scoped identity/idempotency, private files/checksums, poisoning after failed writes, same-account single-flight refresh, cancellation/abandonment, restart without replay, secret exclusion from SQLite and schema-27 migration preserving OpenCode bytes. They do not establish JWT-signature verification, real token rotation, live login/inference or native-release qualification.

Current token-failure diagnostics use IPC 43 with unchanged SQLite 30 (native provider binding originated at IPC 42/SQLite 30; policy controls at IPC 41/SQLite 29; login at IPC 40/SQLite 28). Stop the old server with its matching client before upgrading; a protocol mismatch cannot silently fall back. Do not launch its binaries against retained QA state without a separately approved migration plan; deterministic fixtures do not authorize changing existing diagnostics. [ADR 0030](adr/0030-native-provider-and-task-bindings.md) supplies the reviewed coding/provider/policy binding; owner browser sign-in and a separately approved request budget remain necessary for live qualification.

Synthetic login-control tests additionally cover connection-scoped cancellation, disconnects, slow consumers, abandoned drains, shutdown during admission, committed installation/cancellation races, status and provider-local removal, closed/redacted framing diagnostics, client outcome scope/generation validation with no reconnect/replay, and terminal-safe scrollable URL dialogs that reject paste/image input and clear URLs on cancellation. The old OpenCode hidden-input/removal tests now choose OpenCode in the provider menu; their secrecy and generation assertions remain. No test opens a browser or the production callback port.

[ADR 0032](adr/0032-oauth-callback-extensions-and-diagnostics.md) has a failing-before regression for bounded browser callbacks with extensions and legal query literals. Additional tests retain host/state/duplicate/body limits, bound names/values/field count at the unchanged8KiB request ceiling, reject issuer mismatch and token-bearing hybrids, and verify only fixed rejection reasons/provider-denial text. Actual loopback tests reject callbacks without contacting the token endpoint, then accept a later matching callback and send exactly one unchanged five-field code exchange. An ignored endpoint/model/scope extension cannot select a route or change granted credential data. The owner's earlier generic callback rejection remains causally unclassified; tests establish the independent compatibility defect, not a reconstructed secret-bearing live request.

[ADR 0033](adr/0033-token-response-extensions-and-diagnostics.md) adds failing-before token-response extension/case regressions, nested and escaped duplicates, envelope field/name/body/depth bounds, unchanged scope/lifetime/account checks, fixed-reason transport/claim/terminal tests and closed IPC43 round trips. Actual supervised login failure preserves synthetic credential bytes, exposes only the reason to its connection and makes no second exchange. Successful code exchange/installation/refresh fixtures exercise ignored metadata and case-insensitive Bearer; failed refresh still requires reauthentication without replay. No token-response body from owner testing is used. The generic live invalid-token failure's exact guard remains unknown until a separate owner-performed sample reports a fixed classification.

[ADR 0031](adr/0031-login-browser-and-clipboard-interaction.md) adds synthetic coverage for automatic browser opening exactly once, explicit `o`/`c` and mouse fallback, narrow-layout button hitboxes, cancelled/foreign dialog outcomes, complete long-link copying and hostile helper input. Subprocess fixtures test no-queue admission, clipboard ownership, timeout/cancellation/drain and fixed launcher argument boundaries without opening a real browser or reading/writing the host clipboard. The CLI helper reuses pinned arboard 3.6.1 (Apache-2.0 OR MIT); reviewed `set_text` and macOS/Windows/Linux implementations retain their OS-owned/clipboard-owner semantics. No dependency version or unsafe code is added; Tokio's already-reviewed process feature is explicit for standalone CLI builds.

## Data-use policy admission

```sh
cargo test --workspace --locked data_use -- --test-threads 2
```

[ADR 0029](adr/0029-provider-admission-and-data-use-policy.md) covers the independent four-way policy matrix, owner mutation idempotency/sequence conflicts/restart/quota/corruption, populated schema-28 preservation, known policy failures at prepared root/foreground dispatch, stale maintenance policy binding, actual loopback parent/child policy-change boundaries, typed post-authentication IPC and terminal controls/blocked-model/draft preservation. These are synthetic tests, not assertions about a provider's actual data practices or authorization to upgrade retained QA state.

## Native Codex adapter

```sh
cargo test -p morons-server --lib --locked openai_codex -- --test-threads 2
```

[ADR 0028](adr/0028-native-codex-responses-contract.md) pins the full-Responses GPT-5.5 contract. Tests use only synthetic credentials and loopback HTTP: fixed body/headers, strict tools and normalized images, local-only output limits, policy checks, provider-instance/turn/generation/request-sequence binding, receipt-bound reasoning and sticky-header isolation, lease release after headers, cancellation/drop/deadlines, contradictory/oversized metadata and no retry/fallback. The adapter now has synthetic application-level root, mixed-provider task and compaction coverage. These checks do not qualify subscription entitlement, current account policy, remote generation/spend limits or live interoperability. Durable policy settings guard both provider identities, with exact native provider/credential and cross-provider task bindings; no retained QA upgrade or real browser/provider call is implied.

[ADR 0035](adr/0035-reviewed-native-model-expansion.md) extends the exact native full-Responses matrix to Astra, Sol, Luna, Terra and Daybreak Blue. Tests assert unchanged bodies except the selected ID, local limits/unknown policy, unsupported IDs, provider denial and Blue→Sol response mismatch without fallback/replay. Root image/tool/receipt flows and policy/missing-login/default admission run for all six models. Mixed-provider children include Luna and Blue in both directions, plus Astra→Terra within native credentials; compaction adds Astra foreground and Sol background alongside GPT-5.5. Histories reopen with exact model bindings. CLI checks deliberate selection and Blue's approval disclosure. These fixtures do not establish access or repair the earlier unclassified live protocol failure. No Pi credential or configuration is imported at runtime.

## Native application binding

```sh
cargo test -p morons-server --lib --locked run_supervisor::tests::native -- --test-threads 2
cargo test -p morons-cli --lib --locked native_models
```

Synthetic application tests cover native text/image tools and receipt-bound continuation without an OpenCode key; both mixed-provider directions with deliberately different credential generations and later model-setting changes; wrong/missing/corrupt binding evidence; foreground tools-free turn separation and background installation unaffected by unrelated credential changes; cancellation while waiting for the native lease; replacement invalidating a later turn without replay; reviewed policy/credential admission and explicit picker/local-limit disclosure. Existing migration coverage additionally reopens populated schema-29 OpenCode task history without inventing bindings. These do not verify real login, refresh rotation, subscription entitlement, provider data practices, costs or native release artifacts.

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
