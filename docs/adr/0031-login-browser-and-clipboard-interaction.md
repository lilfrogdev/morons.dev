# ADR 0031: Usable native login links

## Status

Accepted 2026-09-07 following owner testing: the wrapped, mouse-captured terminal URL was not practically copyable. The owner requested automatic browser opening and copy/click fallbacks. This supersedes ADR 0019's manual-only browser interaction, not its OAuth, custody, attribution or fixed routing contract.

## Boundary change

After deliberate login confirmation, the client automatically submits the **authenticated server's validated, fixed OpenAI authorization URL** to the local default-browser handler once. There is no browser launch from a prompt, tool result, model/catalog field or arbitrary hyperlink. No Morons-constructed shell source, `BROWSER` command expansion, user-selectable launcher, token import or browser-profile access is added.

The browser navigation URL contains ephemeral state/challenge, not bearer tokens, codes or the PKCE verifier. Unlike the earlier manual-only rule, it may now occur in an OS launcher's process arguments and browser history. The reviewed concrete launchers are `/usr/bin/open` on macOS, `/usr/bin/xdg-open` on Linux, and System32 `rundll32.exe` with the absolute System32 `url.dll,FileProtocolHandler` on Windows. Windows uses an absolute OS SystemRoot; the launcher working directory is its system directory. Launcher output is discarded and the URL is never put in shell source, logs, model context, ordinary status or Morons files. OS helpers and the selected browser remain trusted-local desktop components, not isolation boundaries. Missing/rejected/timed-out launch reports only a fixed classification and never starts a new OAuth exchange automatically.

Explicit **Copy link** writes the whole URL to the OS clipboard, including on terminals that capture the mouse or wrap lines. It does not read the existing clipboard, copy automatically, use OSC52, or paste into Morons. Other applications/clipboard managers or cloud clipboard sync may retain it; cancellation/completion does not promise erasure or clear unrelated clipboard contents. A bounded self-helper receives only the validated navigation URL through stdin (not its arguments), reusing pinned arboard. Keeping that helper alive while login is visible retains X11/Wayland clipboard ownership. End/cancel/shutdown closes its ownership, but clipboard managers may retain copies.

## Ownership and presentation

- One automatic open for the first URL of the currently waiting login; duplicate/late start events cannot reopen a browser or revive a cancelled dialog.
- `o`/Enter and a clickable **Open browser** button explicitly resubmit the same URL; `c` and a clickable **Copy link** button explicitly copy it. Plain left-click or Ctrl+click works when the terminal forwards mouse events; keyboard controls do not depend on terminal hyperlink support. No arbitrary terminal escape/hyperlink rendering is introduced. Buttons are outside the scrollable URL area and hitboxes are refreshed/cleared with layout and dialog lifetime.
- One nonqueued browser launch and one clipboard helper at a time. Each is bound to a private local dialog registration, not a client mutation ID or URL equality. Late outcomes cannot update another attempt. Subprocess startup/acknowledgement is bounded to five seconds; cancellation owns termination/reaping with a five-second drain bound. Clipboard ownership lasts at most 680 seconds in the client, and a 690-second helper-local watchdog prevents a parent crash plus a blocked platform call/destructor from leaving it indefinitely alive. A browser already handed off may remain open and a clipboard operation may already have occurred. No rollback, successful sign-in or model-selection claim follows a successful launcher exit.
- Login completion, cancellation, disconnect and shutdown invalidate link controls and cancel helper ownership. No helper can delay or retry the server's token exchange. The existing dedicated authenticated login connection and server supervisor are unchanged.
- Browser/clipboard result messages remain fixed and confined to the auth dialog. The stale 'coding integration is not enabled yet' text is removed; login still does not select a model.

## Qualification

Use synthetic URLs and injected subprocess/clipboard fixtures for automatic-once, explicit fallback, scoped late-event rejection, cancellation/timeout/drain, fixed argument boundaries, complete URL copying, hostile/oversized input, redaction, narrow rendering and click hitboxes. Do not launch a real browser or change the host clipboard from tests/CI. Reuse the existing dependency versions and no unsafe code. Run full local/platform/security gates before another owner login sample. Live desktop/browser behavior and real OAuth remain separate qualification; retain previous failed QA state and captures outside Git.
