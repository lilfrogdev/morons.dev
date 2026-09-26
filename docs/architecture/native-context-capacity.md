# Native ChatGPT context capacity

The reviewed native ChatGPT models use the default context policy from OpenAI
Codex commit `25270df2615eb4da5b9d4a9a392226933fb096c5`:
`codex-rs/models-manager/models.json` and
`codex-rs/protocol/src/openai_models.rs` (`usable_context_window` and
`auto_compact_token_limit`). The default window is 272,000 tokens, usable input
is 258,400 (95%), and foreground automatic token pressure begins at 244,800 (90%
of the window). This does not enable the optional 872,000-token context override.

Background compaction retains its separate early-pressure policy: 70% of usable
input minus one eighth of that threshold (headroom clamped to 8,192–32,000).
For the new budget, its estimated-token trigger is 158,270, not 244,800.
When enabled, it prepares after a successful run while the session is idle;
independent byte, entry, and image pressure can trigger it earlier. A useful,
safe compaction prefix and the existing admission checks are still required.

Only new native ChatGPT run selections receive the larger input limit. The
32,000-token local output acceptance bound is unchanged; it is not sent as an
output cap to the native backend. Other provider and hosted-search limits are
unchanged. Independent source-byte, entry, image, tool-output, compaction-count,
and request-size bounds can still require earlier compaction or reject a run.
Provider usage remains provenance-checked, with conservative byte accounting
when trustworthy usage is unavailable. No effects are retried by this change.

SQLite schema 47 rebuilds the run, accepted-run, task-binding, and provider-fact
tables to permit the larger native input budget. The larger bound is restricted
to the reviewed native model names and Responses revision 5. Existing rows and
accepted budgets are copied unchanged; the normal pre-migration database backup
and integrity checks apply. Existing 96,000-token runs retain their old token
pressure policy. No checkpoint, digest, credential binding, or transcript is
rewritten. Binaries supporting only schema 46 cannot reopen an upgraded database.

The new budget is not an entitlement guarantee or a sandbox. The fixed reviewed
manifest remains authoritative; remote model metadata cannot raise local limits.
