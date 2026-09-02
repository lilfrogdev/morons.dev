# ADR 0001: Local IPC endpoint authentication and authorization

## Status

Accepted as amended by ADR 0012

## Context

The server owns trusted state and future execution capabilities. A valid application protocol handshake proves compatibility but does not prove that a client is authorized or that a client connected to the legitimate server.

A fixed Unix-socket or named-pipe name permits endpoint impersonation and, on Windows, can permit pre-creation before the legitimate server starts. Operating-system controls can authorize clients of the legitimate listener, but they do not independently authenticate a server reached through a stale or substituted endpoint.

A private randomized endpoint registration reduces impersonation opportunities but does not provide cryptographic mutual authentication. Sending a persistent secret as an identity response would authenticate the server under the stated operating-system-user boundary, but it would unnecessarily transmit long-lived authentication material.

The implementation must support macOS, Linux, and Windows without adding workspace-owned unsafe code. Authentication must finish before application protocol messages or sensitive data are exchanged. Malicious processes already running as the same operating-system user remain outside this boundary.

## Decision

Local IPC will combine operating-system peer authorization, randomized endpoint registration, and role-separated HMAC-SHA256 proofs. The application protocol handshake remains independent and runs only after local transport authentication succeeds.

### Authentication key and Host Epoch

Each control root will receive a cryptographically random 256-bit local authentication key when it is initialized. The key is persistent and stored under exclusive owner access. Automatic creation is allowed only while securely creating a control root that did not already exist. An existing control root with an unreadable, malformed, missing, or insecure key must fail closed rather than generate a replacement.

Key replacement is an explicit offline whole-control-root reinitialization. It must stop the server, remove the old control directory and associated registration, install a fresh key and stable lock under the same owner-only controls, and start the next server with a new Host Epoch. Removing or replacing only the key fails closed. The implementation accepts only the current key and provides no silent fallback to an older key.

Each server process will generate a new cryptographically random 128-bit Host Epoch. The Host Epoch identifies one server generation and prevents stale lifecycle state from being accepted as current.

The key must never cross IPC or appear in registrations, endpoint names, logs, audit events, prompts, subprocess environments, or sandbox files.

### Host ownership

The server must acquire an exclusive operating-system-backed lock on a stable owner-only lock file before initializing or changing control state. It retains the lock until shutdown. Failure to acquire the lock means another startup or server may own the control root, so the process must not publish, repair, or remove control state.

The lock file must be created and validated under the same ownership and link-safety rules as other control files. Normal startup, stale-state cleanup, and shutdown must never remove or replace it because replacing a locked inode or file can permit two processes to hold locks on different objects at the same path. The operating system releases the lock when the owning process exits, including after a crash.

Only the lock holder may initialize the authentication key, classify registration state as stale, create a listener, publish a registration, or perform identity-bound cleanup. A process ID file or create-once sentinel is not a substitute for the held operating-system lock.

### Control directory and registration

The server will maintain an owner-only control directory outside untrusted workspaces. Existing directories and files must be verified for ownership, access controls, and unsafe links before use.

The endpoint registration will use a bounded, strict schema containing:

- a registration schema version;
- the authentication protocol version;
- the Host Epoch;
- the platform endpoint name;
- the server process ID.

The process ID supports lifecycle diagnostics and an additional consistency check but is never sufficient authentication evidence.

Registration publication will write and synchronize an owner-only temporary file, then atomically rename it to the absent registration path in the same control directory. Only the host-lock owner may publish, and only after creating the listener, installing its access controls, and verifying those controls. Connections admitted before publication is complete will be rejected.

During normal shutdown, the server may remove registration state only when it still names that server's Host Epoch and endpoint. After a crash, a successor holding the exclusive host lock may remove a stale registration only after validating owner control, strict schema conformance, and endpoint confinement beneath the expected runtime root. To recover from a crash between listener creation and registration publication, the lock holder may also remove owner-owned Unix sockets matching the complete endpoint grammar inside the dedicated runtime directory. A stale process must never delete a successor's registration or an arbitrary path named by malformed state.

### Endpoint lifecycle

Every server process will use a new platform-safe endpoint derived from its Host Epoch rather than a fixed endpoint name.

On Unix, the socket will reside beneath an owner-controlled mode `0700` runtime directory. The socket must be owned by the server's effective user and use mode `0600` before registration is published.

On Windows, the root and control directories use a verified protected DACL granting inheritable full control only to the current user and LocalSystem. Authentication keys, host locks, and registrations are ordinary children created only after the directory is hardened, so they inherit no access for untrusted principals. The named pipe uses the protected owner-only SDDL `D:P(A;;GA;;;OW)`, installed when the listener is created rather than repaired after it becomes reachable.

A process that cannot acquire the host lock must refuse to start regardless of registration contents. Once a process holds the lock, a remaining registration cannot belong to a simultaneously valid predecessor and is handled as constrained stale state. Recovery creates a fresh Host Epoch and endpoint rather than reusing the stale endpoint.

### Mutual authentication protocol

The authentication protocol uses:

- a 256-bit HMAC-SHA256 key;
- a fresh 256-bit server nonce for every accepted connection;
- a fresh 256-bit client nonce for every connection attempt;
- the registered Host Epoch;
- an independent authentication protocol version.

The HMAC input is this fixed binary sequence:

```text
"morons.dev/local-ipc-auth" || role || auth_version || host_epoch || server_nonce || client_nonce
```

The context is the exact ASCII byte sequence shown without a terminator. `role` is one byte: `0x01` for the client proof and `0x02` for the server proof. `auth_version` is an unsigned 32-bit integer in big-endian order. `host_epoch` is exactly 16 bytes, and each nonce is exactly 32 bytes. The fixed field sizes make the encoding unambiguous and ensure that a client proof cannot be reflected as a valid server proof.

Authentication records use the existing four-byte big-endian length prefix with a separate maximum payload of 65 bytes. Their payloads are strict binary records:

```text
server_challenge = 0x01 || auth_version[4] || host_epoch[16] || server_nonce[32]
client_proof     = 0x02 || client_nonce[32] || proof[32]
server_proof     = 0x03 || proof[32]
```

Each record must have exactly the length implied by its tag. Unknown tags, trailing bytes, wrong lengths, and out-of-order records are authentication failures.

The connection sequence is:

1. The client reads and strictly validates the owner-only registration and local authentication key.
2. The client connects to the registered endpoint within a bounded deadline.
3. On Unix, both peers verify that the connected peer's effective user ID equals their own.
4. On Windows, the client compares the connected server process ID with the registration when available, without treating the process ID as sufficient proof.
5. The server authorizes the operating-system peer, generates a server nonce, and sends a bounded challenge containing the authentication protocol version, Host Epoch, and server nonce.
6. The client verifies the challenge against the registration, generates a client nonce, and sends the nonce with the client-role HMAC proof.
7. The server verifies the client proof with a constant-time API and sends the server-role HMAC proof.
8. The client verifies the server proof with a constant-time API.
9. Both processes discard nonces and proof buffers when the attempt finishes.
10. The application protocol handshake begins only after every authentication check succeeds.

Proofs are bound to both fresh nonces, the Host Epoch, and the authentication protocol version. A captured proof is therefore invalid for another role, connection, server generation, or authentication protocol version.

Authentication records are separate from `ClientMessage` and `ServerMessage`, use a smaller independent size limit, and have a non-resetting deadline. Authentication failure closes the connection without an application protocol response.

## Implementation

- Use `getrandom` for operating-system randomness and RustCrypto `hmac` with `sha2` for HMAC-SHA256.
- Use `interprocess` peer credentials for Unix effective-user-ID checks and Windows server process IDs.
- Use `rustix` to obtain the Unix effective user ID.
- Use the standard library's operating-system-backed `File::try_lock` and retain the file handle for the server lifetime.
- Use `fence-windows` to locate Local AppData and harden and verify Windows control directories without following reparse points.
- Use `interprocess` security-descriptor support and `widestring` to install the Windows named-pipe DACL at creation.
- Require Unix `HOME` to be an absolute owner-controlled directory before using `~/.morons/control`, and use Local AppData under `morons.dev/control` on Windows.
- Keep randomized Unix sockets under `~/.morons/run` and derive Windows named-pipe names directly from the Host Epoch.
- Do not shell out to PowerShell or add workspace-owned unsafe code.
- Pin every direct dependency exactly and review its source, license, and transitive dependency impact before adoption.
- Fail server startup when required ownership, permissions, DACLs, randomness, key storage, locking, or registration guarantees cannot be established.

## Consequences

- Different local users cannot use the legitimate server's control channel or impersonate it using a predictable endpoint.
- Neither peer accepts the other based only on an endpoint name, process ID, or protocol version.
- The long-lived authentication key is never transmitted over IPC.
- The fixed endpoint is replaced by an atomic registration lookup and a per-process endpoint.
- A lifetime host lock replaces fixed-endpoint binding as the duplicate-server exclusion mechanism.
- Authentication adds a bounded challenge-response exchange before the existing application protocol handshake.
- Crashes may leave stale registration or endpoint artifacts, so cleanup is generation-bound and fail-closed.
- Same-user processes remain able to read the owner-only key and authenticate because they remain inside the operating-system-user trust boundary.
- Unix and Windows implementations require platform-specific tests for valid proof, wrong key, replay, reflection, endpoint impersonation, stale state, startup races, timeout, and cleanup.
- Future privileged operations require untrusted subprocesses to remain isolated from the control directory and host endpoint.

## Alternatives rejected

- Protocol-version fields are not credentials.
- A fixed endpoint protected only by filesystem permissions or a named-pipe DACL does not authenticate the server reached by the client.
- Endpoint or registration secrecy alone is not an authentication boundary.
- Process IDs are reusable and cannot serve as authentication evidence by themselves.
- A process ID file or removable lock sentinel cannot provide lifetime single-host ownership.
- Filesystem permissions alone do not verify an accepted Unix peer.
- Returning a persistent random identity directly would transmit reusable authentication material instead of proving possession of it.
- A single unscoped MAC is vulnerable to cross-role use; client and server proofs require separate role tags.
- Repairing a Windows DACL after listener creation leaves a pre-security connection race.
- Hand-written Win32 FFI would require avoidable workspace-owned unsafe code.
