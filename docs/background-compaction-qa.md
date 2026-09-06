# Background-compaction qualification

Use [ADR 0026](adr/0026-background-compaction-maintenance.md) for the contract and [testing](testing.md) for local gates. Development loopback tests and component timings are not credentialed, platform or release qualification.

## Deterministic checks and measurements

```sh
cargo test -p morons-server --lib --locked maintenance -- --test-threads 2
cargo test -p morons-server --lib --release --locked measure_background_maintenance -- --ignored --nocapture --test-threads 1
```

The timing probe uses disposable state, fake credentials and five fixed histories of five 11,000-byte assistant messages. It injects advisory usage to isolate the scheduling threshold. It measures preparation (including integrity/projection), request encoding, dispatch/result ledger transactions, first-request installation and reopen/recovery separately; no network request is sent. Report medians and ranges outside Git, including build identity and platform. These samples do not establish inference latency, provider caching, billing, large-history scaling or routine reuse/discard rates.

Regressions cover low advisory usage near the independent conservative guard; opt-in and stale preparation; duplicate/uncertain prefix attempts; exact binding and recovery; overlapping or abandoned drain callers; nonqueueing lifecycle admission; late/manual/incompatible readiness; hard receiving-tail rejection; immutable prepared bytes; actual next-request installation; blocked-request deadline/stop/archive and archived-session deletion. The archive test also verifies that deletion of an unarchived session is rejected rather than bypassing lifecycle rules.

## Credentialed development procedure

Obtain explicit approval for billable requests and a bounded request budget first. Use fresh, owner-controlled Morons state and a separate disposable selected directory. Do not import credentials from another application, copy credential files, migrate retained diagnostic state casually, or delete/log out retained QA state. Enter any new key only in the non-echoing authenticated `/login` dialog; do not capture that dialog. No prompts, tool arguments, provider bodies, hidden reasoning, opaque identifiers or credentials belong in diagnostic logs.

1. Record the exact source/build and matching client/server protocol. Launch the isolated home without the opt-in; `/context` must show disabled and normal short input must not create maintenance work.
2. Stop that QA server through its authenticated client. Restart the same isolated home with `MORONS_BACKGROUND_COMPACTION=1`, deliberately authorizing possible charges for unused summaries. Verify `/context` shows the disclosure.
3. Select one exact reviewed service/model. Use a bounded read-only fixture, explicit direct-execution prompts and small final replies to build several complete turns. For example, read distinct numbered text chunks within the 200-line tool bound. Inspect `/context` after each turn; do not alter SQLite usage or source records in a live test. Stop at the approved request budget rather than blindly stuffing history or retrying an uncertain cut. Large jumps can still require foreground compaction.
4. Observe the latest maintenance state, source cutoff and completed usage through `/context` (reopen to refresh). A ready result must not yet change the active checkpoint. Once Ready, submit a small new input; Installed and a matching checkpoint cutoff should be visible afterward. Check the final response for retained requirements, not merely a model claim that compaction worked.
5. In separately budgeted cases, change guidance or the receiving model before the next input; readiness should be discarded, not rerouted. Use explicit `/compact` emphasis to verify it is honored instead of consuming unrelated readiness. Record foreground compactions separately.
6. Exercise new input during dispatch and authenticated stop while busy. Input acceptance must not queue behind inference, but existing credential coordination may delay foreground dispatch while response headers are pending. `Ctrl+X` cancels an exact foreground run, not independent maintenance; stop the isolated server to drain background work. Cancellation cannot retract remote processing or charges.
7. Restart without the flag and verify disabled state and no readiness installation or automatic replay. Archive before deleting a disposable session; verify selected files and guidance remain unchanged. Preserve failed fixtures and captures for diagnosis.

Record bounded status classifications, observed maintenance usage, readiness reuse/discard counts, conservative context relief and foreground waits outside Git. Missing completed usage does not establish zero cost. A sampled success does not qualify every model, platform, startup recovery scenario or cache behavior.

Live qualification requires an authorized login in isolated state. If that is unavailable, report the gate as blocked; do not substitute an existing application's credentials or claim loopback evidence is live QA. Follow the separate [release qualification procedure](release-candidate-qa.md) for exact candidate and signed-tag assets.
