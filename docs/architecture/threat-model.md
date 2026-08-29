# Threat model

## Protected assets

- Server credentials
- Local authentication key and endpoint registration
- Agent and session state
- Tool and execution capabilities
- Repository and project data
- Provider and kernel connections

## Untrusted inputs

- IPC clients
- Authentication and application protocol messages
- Endpoint and registration filesystem state
- Repository content
- Model output
- Commands and subprocesses
- External content

## Trust assumptions

- The operating system correctly enforces process identities, filesystem permissions, and Windows DACLs.
- Root, LocalSystem, administrators, and equivalent privileged accounts are outside the local IPC guarantee.
- Malicious processes already running as the owning operating-system user are outside the local IPC guarantee.
- Untrusted repository processes run without access to host control files or IPC endpoints.

## Local IPC threats

- A different local user connects to the legitimate server.
- A process impersonates the server or occupies a predictable endpoint before startup.
- A process replays a stale registration, endpoint, Host Epoch, process ID, nonce, or authentication proof.
- A crash between listener creation and registration publication leaves an unregistered Unix socket.
- Concurrent server startups race to publish different endpoints or delete a successor's state.
- A process tampers with or replaces the authentication key, host lock, control directory, or registration.
- A symbolic link, reparse point, or path race redirects creation, validation, publication, or cleanup.
- A connection is admitted before endpoint permissions or DACLs are installed and verified.
- A fake server causes the client to send credentials or application data before server authentication.
- A client sends malformed, oversized, partial, replayed, or stalled authentication records or application frames.
- A client disconnects during authentication or framing.
- A client uses a compatible protocol version without being authenticated or authorized.
- Authentication material is exposed through logs, prompts, environments, subprocesses, or sandbox files.
- An untrusted repository process discovers or reaches the host endpoint or authentication key.

## Mitigations

- Authorize the operating-system peer before reading connection bytes or issuing an authentication challenge.
- Use a persistent random 256-bit local key and a fresh random 128-bit Host Epoch for every server process.
- Require one server to hold an operating-system-backed host lock for its lifetime before changing control state.
- Keep a stable owner-only lock file that normal startup, cleanup, and shutdown never replace or remove.
- Use randomized per-epoch endpoints published through an atomic, owner-only registration.
- Use fresh 256-bit client and server nonces with role-separated HMAC-SHA256 proofs.
- Bind each proof to the authentication protocol version, Host Epoch, and both connection nonces.
- Verify proofs with a constant-time API and discard all connection authentication state after success or failure.
- Authenticate the connected server before sending application protocol messages or sensitive data.
- Require an absolute, owner-controlled, non-group-writable, and non-world-writable Unix home directory before deriving control paths.
- Use owner-only Unix directories and sockets with bidirectional effective-user-ID verification.
- Use verified protected current-user-and-LocalSystem DACLs on Windows control directories before creating inheriting control files.
- Install the protected owner-only named-pipe DACL atomically when the listener is created.
- Treat process IDs as advisory and require successful mutual proof.
- Reject insecure ownership, permissions, DACLs, links, malformed control files, unavailable peer identity, and inconsistent registrations.
- Publish registration only after listener security is verified and reject connections admitted before publication.
- Remove current-generation state only when its Host Epoch and endpoint still match the cleaning server.
- Remove registration-bound stale state only while holding the host lock and after validating owner control, registration shape, and endpoint confinement.
- Remove unregistered Unix sockets only when they are owner-owned and match the complete endpoint grammar inside the dedicated runtime directory.
- Apply independent size limits and non-resetting deadlines to authentication, framing, and application handshakes.
- Never treat protocol-version fields as authentication or authorization evidence.
- Keep authoritative security decisions in the server.
- Keep control files and host IPC inaccessible from untrusted execution sandboxes.
- Exclude keys, nonces, proofs, and credentials from logs, audit payloads, prompts, environments, registrations, and endpoint names.

## Residual risks

- Root, LocalSystem, administrators, and equivalent privileged accounts can bypass local controls.
- Processes running as the same operating-system user are not distinguished from the legitimate CLI.
- Process separation alone does not sandbox untrusted commands.
- Compromise of the local authentication key permits impersonation until an explicit offline key replacement invalidates existing registrations and endpoints.
- Operating-system, filesystem, or cryptographic-randomness failures can invalidate the assumptions behind local authentication.
- Authorized same-user processes can deny service by exhausting resources or interfering with lifecycle state.
