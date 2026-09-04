# ADR 0015: Bounded transcript window paging

## Status

Accepted

## Context

The terminal originally loaded a session transcript from its oldest entry forward and stopped after 512 one-entry pages. Sessions with more history could not be opened, while rendering rebuilt every loaded transcript block on every frame. Raising the ceiling would increase startup time and memory use without solving long-history behavior.

Canonical transcript history remains durable in SQLite and session event subscriptions remain the source of live updates. Transcript text is untrusted and individual entries can be large, so paging must preserve the existing frame, text, and result bounds.

## Decision

Application protocol version 32 adds an explicit `older` or `newer` direction to transcript page requests. Responses carry independent opaque cursors for any older and newer page. Every cursor binds the session, transcript high water, event high water, and continuation boundary. A cursor therefore traverses one fixed snapshot; it is a locator, not authorization evidence.

The server returns entries chronologically within each page. A request without a cursor starts at the latest edge for `older` or the oldest edge for `newer`. The maximum wire page remains one entry so one worst-case transcript entry cannot multiply past the bounded IPC frame. No persistence schema change is required.

The terminal composes up to 64 wire pages into one display window. Opening a session loads the newest window first. Page navigation replaces that window rather than accumulating the complete transcript:

- PageUp or upward wheel movement at the window's top loads the adjacent older window.
- PageDown or downward wheel movement at its bottom loads the adjacent newer window.
- Home loads the oldest window directly.
- End reloads the current latest window.

Only 128 live entries may be held between tail refreshes. If live updates fill that bound, the client rotates its in-memory tail and requests a fresh latest window. Queries may use the existing bounded reconnect policy because they have no external effect.

While an older window is visible, subscription events continue advancing but new transcript text and transient deltas are not mixed into the noncontiguous historical window. The UI records newer output, preserves the reader's location, and reloads the latest snapshot when requested. Installing a fresh latest window restarts the gap-free subscription at that snapshot's event cursor so events racing the query are replayed rather than lost.

Wrapped block heights are cached by the viewport. A frame constructs lines only for blocks intersecting the visible rows. All loaded blocks are remeasured only when content or wrapping width changes. This keeps steady-state rendering proportional to the viewport instead of the durable transcript length.

## Consequences

- Sessions remain openable and navigable beyond the former 512-entry client limit.
- Client transcript memory is bounded independently of the durable 100,000-entry session limit.
- Moving across a window edge performs local IPC reads and may briefly show a loading status.
- The scrollbar describes the current bounded window; title arrows disclose whether older or newer windows exist.
- Starting new work from a historical window is blocked until End restores the latest transcript, avoiding a misleading disjoint display.
- Protocol version 31 clients and servers fail the existing version handshake rather than interpreting the new cursor contract.
