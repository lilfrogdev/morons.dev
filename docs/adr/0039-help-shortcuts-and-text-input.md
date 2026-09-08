# ADR 0039: Help shortcuts must not consume text input

## Status and evidence

Accepted before implementation, 2026-09-08. During owner-authorized native image/compaction QA, a question mark opened Help while composing a question. The global `?` handler precedes session, rename and credential text handlers. Later text was consumed by the information dialog. An attempted Escape/Enter close/reopen instead closed Help and submitted the partial draft. Preserve that canonical input and outcome; do not replay or repair history. Model recall succeeded, but input fidelity did not. This is separate from ADR0034's bounded queue delivery.

## Decision

Restrict the printable `?` Help shortcut to the session browser after existing modal, pending-operation and control-key handling. In the session composer, rename input and credential input, `?` follows the same bounded character path as other printable text. Model/settings/auth dialogs keep their own existing handlers. Existing `/help` remains the deliberate composer control; do not introduce another global printable shortcut. Help/Context dismissal remains explicit and does not submit text by itself. Update usage text to distinguish browser `?` from composer `/help`.

No automatic submission, new retry, history mutation, queue enlargement, credential disclosure, new IPC operation or provider change is introduced. Credentials retain their dedicated hidden buffer; regression fixtures use synthetic values only. Prompt and command admission, cancellation, local authority, terminal sanitization, selected-directory semantics and all existing bounds remain unchanged. Escape still acts on the current dialog/view: UI automation must inspect that state before a later Enter rather than assuming Escape always closes a session.

## Validation

Failing-before key-event regressions must cover complete question-bearing user input, both command modes, rename and hidden credential entry. Verify browser Help, explicit composer `/help`, dialog dismissal without submission, and unchanged pending/confirmation handling. Compare full drafts before Enter in live QA; retain the failed partial-input sample. Run all locked local gates and exact-head platform/security checks before live qualification of a rebuilt pair. No dependency, schema or protocol revision is required.
