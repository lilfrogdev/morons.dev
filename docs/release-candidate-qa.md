# Local release-candidate QA

This is the repeatable native smoke gate for a Morons release archive. It supplements CI; it does not replace native target validation from [ADR 0007](adr/0007-supported-processor-architectures.md).

Run the checklist against the exact archive intended for publication, not binaries copied directly from `target/`. Use a disposable home and working directory so the test does not reuse normal Morons control state, credentials, sessions, skills, or model preference.

## Safety and evidence rules

- Use only repositories and files created for this run.
- Treat the tested model, tools, skills, commands, and web results as having the QA account's normal local authority. This is not a sandbox.
- Enter provider credentials only through the non-echoing `/login` dialog (`Ctrl+K` shortcut). Never place them in shell history, Herdr commands, screenshots, logs, or the result report.
- Do not reuse Pi or another application's credentials or state.
- Record stable result classifications and short observations. Do not record credentials, provider bodies, opaque provider identifiers, authentication material, or unnecessary absolute paths.
- A failed or cancelled external effect is never automatically retried. Inspect the disposable directory before deciding whether to run a new test.
- Remove the disposable home, repository, extracted archive, kernels, and companion after recording results.

## Candidate setup

From the exact clean candidate checkout, build and package one reviewed target with:

```sh
./scripts/package-release.sh <target-triple> <output-directory>
```

The helper refuses a dirty checkout, builds the client and server with Cargo's lockfile, downloads only the pinned target-specific uv asset, verifies all three binary formats and the reviewed uv checksum, records all three hashes, and emits an archive plus SHA-256 sidecar.

Record these values before starting:

- source commit and tag candidate;
- Rust target triple;
- archive filename and SHA-256 digest;
- host operating system and native processor architecture;
- Herdr pane identifier;
- managed Python, uv, `jupyter_client`, and `ipykernel` versions observed after first setup;
- whether a Brave Search key is available.

Verify that the source checkout is clean and that the archive digest matches the candidate manifest. Extract into a new directory and verify every manifest entry, including matching `morons`, `morons-server`, and `morons-uv` executables plus both uv license files. On Unix, all three executables must have executable mode. Do not add the extracted directory to `PATH` for this test.

Create isolated state from a Bash-compatible shell:

```sh
export QA_HOME="$(mktemp -d)"
export QA_REPO="$(mktemp -d)"
git -C "$QA_REPO" init
printf 'alpha\nbeta\n' >"$QA_REPO/sample.txt"
mkdir -p "$QA_REPO/.agents/skills/qa-skill"
cat >"$QA_REPO/.agents/skills/qa-skill/SKILL.md" <<'SKILL'
---
name: qa-skill
description: Reply with the exact release QA marker.
---
When invoked, include `QA-SKILL-OK` in the answer.
SKILL
```

Leave `MORONS_PYTHON` unset for the managed-runtime checks. Preserve the ordinary `PATH` and development environment; do not use `env -i`. If testing successful web search, set `BRAVE_SEARCH_API_KEY` only in the environment inherited by the companion and do not print it.

Launch the extracted `morons` from `QA_REPO` in a Herdr pane with `HOME=QA_HOME`. Herdr should only send keys/text and read the visible pane. Long pasted prompts should use bracketed paste. Use `;`, not `&&`, when the pane shell is Nu.

## Checklist

Record every item as `pass`, `fail`, `blocked`, or `not run`. A failure blocks the candidate unless it is understood and fixed in a separately reviewed change.

### Package and startup

| ID | Check |
| --- | --- |
| PKG-01 | Archive digest, target name, contents, and executable modes match the release manifest. |
| PKG-02 | `morons` discovers only its exact sibling `morons-server` and starts it automatically. |
| PKG-03 | First launch shows the trusted-local authority notice before normal interaction. |
| PKG-04 | A copied client without its sibling fails before the TUI with a categorized, actionable, redacted diagnostic. |
| PKG-05 | `/login` configures, replaces, or removes the credential without echoing or later displaying key material; `Ctrl+K` opens the same flow as a shortcut. |
| PKG-06 | `/help` repeats the trust posture, commands, cancellation, and session controls. |
| PKG-07 | The packaged `morons-uv` has the manifest digest and target format, reports uv 0.12.9, and both shipped uv license texts match the reviewed source. |

### Directory and session lifecycle

| ID | Check |
| --- | --- |
| SES-01 | A new session binds `QA_REPO`; `!pwd` reports that directory. |
| SES-02 | A second session created by another client in the same directory shows the shared-directory race warning. |
| SES-03 | Rename survives leaving and reopening the session. |
| SES-04 | Switching sessions and closing one client do not cancel server-owned work. |
| SES-05 | Archive blocks new work and unarchive restores availability. |
| SES-06 | Deletion requires archived state and confirmation, removes Morons-owned history/attachments, and leaves `QA_REPO` and its marker files unchanged. |
| SES-07 | Restart restores remaining sessions and durable transcript entries. |

### Model selection

| ID | Check |
| --- | --- |
| MOD-01 | `/model` opens the complete searchable picker with the current model and data-use disclosure. |
| MOD-02 | `/model go grok` ranks the reviewed Go/Grok pair first; further typing, Up/Down, Enter, and Esc work. |
| MOD-03 | Tab and Shift+Tab do not change the selected model. |
| MOD-04 | A saved model becomes the default for a newly created session and survives client and companion restart. |
| MOD-05 | In a two-client test, a later selection in client A does not alter client B's already-open composer; after B opens or creates a session, B reloads the new global default. |
| MOD-06 | Every selected/default model remains an available reviewed service/model pair; unavailable saved state falls back visibly. |
| MOD-07 | `/model go glm` exposes reviewed Go `glm-5.3-flash` with the expected data-use disclosure and persists it as the global default. |

### Settings

| ID | Check |
| --- | --- |
| SET-01 | `/settings` shows a typed **Subagent model** row; Enter opens a bounded searchable picker and Esc returns without mutation. |
| SET-02 | **Inherit parent** is the initial policy and remains independent from `/model` selection. |
| SET-03 | With Zen `gpt-5.6-sol` selected for the main run and Go `glm-5.3-flash` selected for subagents, `task` routes children through Chat Completions and reports `OpenCode Go / glm-5.3-flash · protocol revision 2`. |
| SET-04 | The exact child setting survives client and companion restart; an unavailable or unauthorized saved pair fails clearly without falling back or rewriting the setting. |
| SET-05 | Returning to **Inherit parent** makes later task calls use the main run's exact selected service/model while an already running task remains pinned. |

### Inference, tools, and context

| ID | Check |
| --- | --- |
| RUN-01 | Go/Grok completes a plain response and a natural `read` request for `sample.txt`. |
| RUN-02 | Natural `write`, `edit`, and `bash` calls change only the intended disposable files and commit bounded results. |
| RUN-03 | A response after a tool loop appears immediately without requiring another event or reload. |
| RUN-04 | Long wrapped output follows the bottom until wheel/PageUp history scrolling begins; new output does not steal that viewport, and End resumes the latest output. |
| RUN-05 | `!command` is durable and context-bearing; `!!command` is durable but excluded from later provider context. |
| RUN-06 | `@qa-skill` invokes the exact project skill and returns `QA-SKILL-OK`; Tab completes only a visible exact skill match. |
| RUN-07 | Two disjoint `task` children run concurrently, cannot recurse or use IPython, and return input-ordered bounded reports. |
| RUN-08 | First IPython use automatically prepares Python 3.11.15 with `jupyter_client` 8.6.3 and `ipykernel` 6.30.1, evaluates a value, preserves it across cells/runs, starts in `QA_REPO`, and renders a traceback without ANSI fragments. |
| RUN-09 | After successful setup, a companion restart reuses the validated runtime with network unavailable and without invoking `morons-uv`; a stale manifest is rejected and rebuilt under the lock when reviewed sources are available. |
| RUN-10 | `/context` reports the reviewed model limit, threshold, reserves, and current checkpoint. |
| RUN-11 | `/compact <instructions>` commits a bounded source-bound checkpoint and continues without deleting canonical history. |
| RUN-12 | Successful `web_search` returns bounded cited results when a key is available. Without a key, it fails as `CredentialNotConfigured` without network fallback. |
| RUN-13 | A transcript exceeding 512 entries opens at its latest window; PageUp/wheel crosses older windows, Home reaches the first entry, PageDown returns through newer windows, and End restores current live output without unbounded rendering. |
| RUN-14 | An explicit `MORONS_PYTHON` lacking Jupyter packages fails with actionable guidance naming `jupyter_client`, `ipykernel`, and `MORONS_PYTHON`. |
| RUN-15 | Go `glm-5.3-flash` completes plain text and a natural `read` tool loop through Chat Completions, with bounded reasoning ignored and no duplicate terminal output. |

### Images

| ID | Check |
| --- | --- |
| IMG-01 | A valid PNG pasted by explicit path becomes one atomic sanitized filename marker. |
| IMG-02 | Duplicate names receive stable suffixes and unsupported/malformed images fail without corrupting the draft. |
| IMG-03 | Go/Grok rejects image submission clearly while retaining the draft and attachment. |
| IMG-04 | Go/Luna accepts the same normalized image and completes an image-aware response. |
| IMG-05 | A Luna `read` of an image produces bounded dimensions, type, byte count, and a usable multimodal result. |

### Cancellation, recovery, and terminal behavior

| ID | Check |
| --- | --- |
| LIFE-01 | `Ctrl+X` cancels the exact active run/command, stops descendants, and leaves completed filesystem effects visible rather than claiming rollback. |
| LIFE-02 | `Esc` detaches from an active run without cancelling it; reopening shows the durable outcome. |
| LIFE-03 | `Ctrl+S` requires confirmation, stops the companion, interrupts active work durably, and restores terminal ownership. |
| LIFE-04 | After forced companion termination, restart marks interrupted work terminal and never replays provider, command, Python, web, or filesystem effects. |
| LIFE-05 | ANSI-colored command output and IPython tracebacks render as plain text with no control-sequence fragments. |
| LIFE-06 | Resizing through narrow and short dimensions reflows wrapped text, preserves the history entry anchor, keeps the growing composer bottom-docked, and does not panic or expose raw controls. |

### Expected separately recorded limits

These are not silently treated as passes:

- automatic compaction at the seventy-percent threshold may be `not run` locally when filling context safely is impractical; deterministic tests and CI evidence must be linked;
- successful web search may be `blocked` when no Brave key is available; the missing-key path must still pass;
- native Intel macOS remains `blocked` until run on reviewed `x86_64-apple-darwin` hardware; cross-compilation is not a substitute.

## Result record

Copy this section into a dated review artifact or release issue. Do not put secrets or raw provider payloads in it.

```text
Candidate commit:
Candidate tag:
Target triple:
Archive:
SHA-256:
Host OS and architecture:
Herdr pane:
Python and Jupyter versions:
Brave success path available: yes/no
Started (UTC):
Finished (UTC):

ID      RESULT    EVIDENCE / SHORT OBSERVATION
PKG-01
PKG-02
...
LIFE-06

Blocked/not-run justification:
Unexpected effects inspected:
Cleanup confirmed:
Reviewer:
```

## Cleanup

1. Archive and delete disposable sessions through the TUI where that behavior is under test.
2. Stop the companion with `Ctrl+S` and confirm no `morons`, `morons-server`, kernel, or test descendant remains.
3. Restore the Herdr pane to the source repository or another known directory.
4. Remove `QA_HOME`, `QA_REPO`, and the extracted archive directory using exact recorded temporary paths.
5. Confirm the source checkout is still clean and the selected working directory was not removed by session deletion.
6. Record cleanup as part of the result artifact. Deletion is not represented as forensic erasure.
