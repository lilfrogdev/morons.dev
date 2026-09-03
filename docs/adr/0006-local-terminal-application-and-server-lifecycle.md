# ADR 0006: Local terminal application and server lifecycle

## Status

Accepted as amended by ADRs 0012 and 0014

## Context

The authenticated server can durably execute one provider-only run, but the current `morons` binary only proves that it can connect and negotiate a protocol version. A usable application needs one permanent interaction path for starting or finding the server, configuring the OpenCode credential, selecting a session and reviewed model, submitting input, observing durable and ephemeral run state, cancelling an exact run, and reconnecting after either process exits.

The terminal renders untrusted user and provider text. Terminal control sequences, bidirectional formatting controls, oversized paste input, connection loss, duplicate input submission, and server-start races must not bypass the application or local IPC boundaries. Credential entry must remain non-echoing and must not create a second credential store in the client.

The server must continue to own sessions and runs independently of the terminal process. Making terminal exit stop the server would break background execution and multi-client observation. Requiring users to start `morons-server` manually would leave the intended local application incomplete.

## Decision

### Product surface

Running `morons` without arguments opens the Ratatui application. The initial permanent interface contains:

- connection and startup status;
- non-echoing OpenCode credential configuration, replacement, removal, and non-secret status;
- a session browser with creation and resume;
- explicit OpenCode Zen or OpenCode Go service and reviewed-model selection with data-use and retention disclosure;
- a session view with canonical transcript history, active-run status, transient assistant text, input, and exact-run cancellation; and
- an explicit confirmed action to stop the local server.

The first implementation renders bounded plain text and preserves line structure without interpreting model text as terminal markup. Rich Markdown rendering is not required for this slice. `morons` requires interactive terminal input and output and fails cleanly before entering terminal mode when either is unavailable; it does not fall back to a raw command or line protocol.

The `morons-server` executable is an internal companion process, not a user-facing command surface. Distribution artifacts contain `morons` and the matching `morons-server` for one operating-system and processor target. Public command names for automation, raw server administration, repository execution, and future workflows remain undefined.

The terminal client owns presentation and temporary interaction state only. It cannot author assistant messages, run transitions, provider outcomes, tool facts, credential status, model availability, or session ownership. The model selector reads a sanitized server application query over authenticated IPC; the client does not fetch or interpret the remote provider catalog itself. ADR 0014 adds one server-owned durable global default-model preference selected through `/model [search]`; it is convenience state rather than authorization or run attribution. The selected service and model remain explicit fields on every submitted run and are never inferred from client-local attachment state.

### Connect or start

The client first attempts to load, connect to, and authenticate the currently registered server. Application data, credentials, and protocol messages are not sent until operating-system peer authorization and mutual authentication complete.

Automatic startup is permitted only for an absent or unavailable server state that can be distinguished from insecure or malformed control state. The client must not start a replacement after an authentication failure, an insecure control path, a missing key in an existing control root, an invalid registration, or a protocol mismatch. Those states fail closed with a sanitized recovery message.

When startup is allowed, the client launches only the exact sibling `morons-server` executable from its installed application bundle or binary directory. It does not search `PATH`, invoke a shell, accept a server path from configuration or repository content, or pass provider credentials, local authentication material, repository content, or capabilities through arguments or environment variables.

The child receives a reviewed minimal environment required by the supported operating system. Repository, tool, provider, proxy, certificate-override, dynamic-loader, and credential variables are not inherited. Standard input, output, and error are not used as an authentication, readiness, or application channel.

Server readiness is established only by loading a newly published owner-controlled registration, connecting to its endpoint, authorizing the operating-system peer, completing mutual proof, and negotiating the application protocol. A child process identifier, exit status, inherited file handle, or readiness string is never server-authentication evidence.

Concurrent clients may race to start the server. Each may launch a contender, but only the process that acquires the lifetime host lock may initialize control state and publish an endpoint. Losing contenders exit without modifying the winner's registration. Clients retry authenticated discovery within one bounded startup deadline and do not delete or repair control state themselves.

### Server and client lifetime

Closing the terminal detaches that client. It does not cancel a run, archive a session, remove a registration, or terminate the server. The server remains available for background runs and other authenticated local clients.

Stopping the server is a deliberate authenticated local-owner application mutation with a stable request identifier and explicit terminal confirmation. The server commits the idempotency result and a non-secret audit fact before acknowledging the request and signaling shutdown. Only the first accepted application result signals the current supervisor. An exact retry returns the prior accepted result and must never stop a successor generation.

After the acceptance transaction commits, the server stops admitting new run input, terminates controlled execution through the existing graceful-shutdown contract, closes client subscriptions, removes only its current-generation registration, and exits. A client must not signal or kill a process merely because its identifier appears in a registration. A committed stop request followed by a crash is recovered as an already accepted request rather than replayed against the next server.

Unexpected server exit is handled as detachment. The client discards ephemeral state, reconnects through authenticated discovery, and reloads authoritative state. Startup recovery, not the client, classifies unfinished runs.

A protocol-version mismatch does not cause automatic replacement. The client reports that the existing server must be stopped with a compatible client or by an explicit operating-system action before the new version can start.

### Connection and retry model

The terminal uses one authenticated request connection, one dedicated authenticated session-catalog subscription while the browser is active, and one dedicated authenticated subscription for the selected session. It opens a new session subscription when the selection changes and closes the old subscription without affecting session or run lifetime. Subscription connections carry one scoped stream and cannot invoke unrelated commands.

Every retriable user-input, cancellation, session-creation, and server-stop mutation retains one generated mutation request identifier until the server returns a committed result or the user abandons the pending operation. Reconnection may resend that exact normalized mutation. It must not create a replacement identifier after an unknown outcome.

A credential-bearing mutation is never resent automatically after connection loss or an unknown outcome. The client discards and zeroizes its transient secret buffer, reconnects, reads non-secret credential status and generation, and requires a deliberate new credential entry when the intended state was not established.

The client maintains bounded connection, authentication, handshake, request, startup, and reconnect deadlines. Reconnect backoff is bounded and does not block terminal input or grow unbounded task state. Presentation errors do not alter durable state.

### Session snapshot and subscription

The server implements the snapshot and subscription contract selected in ADR 0005. The first transcript page fixes the session-entry and durable-event high waters and returns the scoped event cursor from the same transaction. Remaining pages use that fixed snapshot. Subscription replay begins strictly after the returned event cursor before switching to live delivery.

Durable session events contain sanitized committed user messages, completed assistant messages, run transitions, cancellation intent, recovery outcomes, and terminal failures. They are reconstructed from canonical facts and delivery projections rather than stored protocol objects or provider payloads.

Ephemeral assistant deltas contain only the session identifier, exact run identifier, and a run-local monotonic sequence with bounded text. They are emitted only after the durable active transition, are never stored or replayed, and may be dropped or coalesced under backpressure. The client ignores duplicate or decreasing sequences and tolerates gaps because partial text is non-authoritative. A complete committed assistant event replaces all displayed partial text for that run.

Each subscription has bounded replay pages, write deadlines, and an independent bounded live queue. A slow client is disconnected. On reconnect, the client resumes from its last received durable cursor or obtains a new snapshot after a stale-cursor response; it never reconstructs history from ephemeral deltas.

### Credential interaction

Credential entry occurs only inside the authenticated terminal application. The widget does not echo characters, render the secret, retain an input history, copy it to the clipboard, write it to client configuration, include it in panic or debug output, or place it in command arguments or environment variables.

The client stores the in-progress credential only in a bounded zeroizing buffer. Cancelling the form, receiving an application result, losing the connection, leaving the form, or exiting the client clears that owned buffer. The credential crosses IPC only through the existing redacted credential type after server authentication and protocol negotiation.

Credential status displays only configured or unconfigured state and the non-secret generation needed for compare-and-swap mutations. The application never offers credential retrieval or reveal.

### Terminal safety and presentation

All user text, provider text, identifiers, errors, catalog metadata, and future tool output are untrusted presentation input. A single terminal-safety module converts them into bounded Ratatui cells without forwarding embedded C0 or C1 controls, escape sequences, operating-system commands, device-control strings, hyperlinks, terminal-title commands, or bidirectional formatting controls. Newlines are layout boundaries, and tabs are expanded under a fixed bound.

Untrusted text is not written directly with `print!`, `println!`, raw ANSI output, or terminal-backend escape APIs. Trusted terminal-control output is produced only by the reviewed Ratatui backend. Presentation strings remain client-owned and are separate from protocol errors and persistent values.

Terminal setup uses an ownership guard that restores the prior screen and input mode on every ordinary return and handled error path. Panic restoration is best effort and must not print credential or transcript buffers. Terminal resize, paste, key, and mouse events are bounded before entering application state.

The Ratatui application is not a terminal emulator, PTY, shell, editor, or raw sandbox view. It does not accept arbitrary user commands or expose server filesystem, process, provider, storage, or credential operations.

### Limits and testing

The client bounds transcript pages, rendered cells, paste size, input size, queued UI events, reconnect attempts, and transient delta accumulation independently of server limits. Server validation remains authoritative even when the client has already applied a matching presentation limit.

Tests use Ratatui's in-memory backend for deterministic state and rendering behavior and real authenticated local IPC for lifecycle and subscription boundaries. They cover:

- absent-server startup, concurrent startup, stale registration recovery, malformed control state, fake companion paths, child failure, and startup deadlines;
- proof that no application or credential bytes precede server authentication;
- session snapshot and replay races, stale and cross-session cursors, delta loss, durable replacement, reconnect, and slow consumers;
- exact mutation reuse across response loss and prohibition of automatic credential retries;
- terminal-control and bidirectional-text injection, large paste input, resize storms, render bounds, and terminal restoration;
- client exit during an active run and deliberate graceful server stop; and
- every supported operating-system and processor architecture required by ADR 0007.

## Consequences

- One `morons` invocation reaches a permanent local interaction surface without requiring manual server startup.
- The server remains authoritative and long-lived while terminal clients attach and detach freely.
- Durable replay and ephemeral deltas provide responsive output without making partial text recoverable state.
- Credential configuration becomes usable without introducing command-line, environment, or client-side secret storage.
- Server auto-start adds process discovery, race, environment, packaging, and upgrade behavior that requires platform-specific validation.
- Ratatui and its terminal backend become reviewed pinned dependencies of the client only; application and server behavior remain independent of them.
- Coding tools, repository import, sandboxed process execution, web search, skills, kernels, and raw terminal access remain unavailable.
