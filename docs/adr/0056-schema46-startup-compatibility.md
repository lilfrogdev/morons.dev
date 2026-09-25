# ADR 0056: Restricted schema46 compatibility and startup failure reporting

## Decision

Main accepts SQLite46's exact additive migration from the local steering prototype: nullable `skill_context` and `skill_context_digest` columns on `steering_mutation_requests`. Existing schema45 profiles migrate forward with the usual database-only backup; existing schema46 profiles are not downgraded or rewritten to masquerade as schema45. IPC remains 48.

This is storage compatibility, not implementation of steering-skill delivery. Main writes legacy steering records with both reserved columns NULL. Before projection repair or execution recovery, startup rejects any record with either column populated. The prototype's skill snapshots, fingerprints and delivery semantics are not silently ignored. Populated prototype records require a separately reviewed implementation. Existing canonical facts and digests remain unchanged; no credential, provider or tool fallback is added.

## Security and lifecycle boundaries

Keep fail-closed database validation. Never repair compatibility by lowering `user_version`, deleting canonical history, or bypassing credential-generation checks. Database-only QA copies without the matching credential state cannot establish complete-profile startup compatibility.

The CLI detects when its launched companion exits without another registered or initializing server. It returns a fixed startup-exit diagnostic instead of a timeout. Concurrent clients may legitimately lose the companion-launch race, so an exited child must not invalidate a ready or initializing competing server. Allow the existing two-second incomplete-control grace for the winner to create/publish its stable lock before diagnosing a child exit; a transient absent/incomplete discovery is not conclusive. No automatic relaunch is added. Raw child stderr, credential bytes, database values and arbitrary error strings remain excluded from the diagnostic. Direct invocation of the matching companion is an operator diagnostic, not an automatic retry.

## Verification

Regression tests cover schema45 migration and backup, reopening schema46 with legacy steering history, preservation of canonical fingerprints, rejection of populated reserved columns without erasure, and unexpected companion exit. Existing lifecycle concurrency/readiness tests remain required. Full startup qualification uses a disposable complete profile with networking disabled; credentials must not appear in logs and temporary copies are removed after the stopped test.

Historical-validation latency is a separate issue. Do not remove integrity checks or weaken recovery to shorten startup. Measure stage timings before choosing a bounded optimization.
