# rsi-meta security boundary

## Native trust domain

Native plugins are trusted host-level code. Capabilities restrict requests made through host handles, but they do not stop plugin code from reading process memory, accessing the operating system, starting threads, aborting, or corrupting the daemon. `rsi-meta` does not provide a process sandbox or Wasm isolation mode.

The [loader](../loader/README.md) validates package identity, target, hashes, file types, ownership-sensitive inputs, and configuration before mapping a library. The lock pins the selected top-level library, not every dynamic dependency that the operating-system loader may resolve; deployments must treat that dependency environment as trusted. These checks establish package integrity; they do not turn native code into an untrusted security boundary.

## Local transports

The daemon accepts Unix sockets and loopback HTTP/WebSocket only. It rejects non-loopback binds before opening durable host state or creating credentials.

The runtime directory and Unix socket must be owned by the effective user with restrictive modes. The server checks peer credentials; clients verify that the socket is a non-symlink socket with the expected owner and mode. Unix transport remains the recovery path after bearer-token rotation.

HTTP and WebSocket requests other than anonymous health/version endpoints require an exact bearer token. Native clients may omit `Origin`; a supplied origin must exactly match the configured allowlist.

## Credentials and secrets

The token file is an owner-checked, non-symlink regular file published through a durable same-directory replacement. Rotation allocates a durable generation before changing the file, returns the initiating outcome, then closes existing WebSockets. Startup repairs a token file behind durable state and fails closed when a token file is ahead.

Secret references are resolved by the loader and remain outside canonical manifests and locks. Plaintext secret injection, redacted audit values, and file/keyring input checks are defined by the [configuration reference](subsystems/configuration.md).

## Bounds and failure containment

Ingress, control egress, plugin control/data lanes, frames, and outstanding stream credit are bounded independently. Native plugin artifacts are capped at 256 MiB, and package-relative artifact/schema paths cannot escape through a symlinked parent. A saturated data lane cannot consume lifecycle-control capacity. Exact transport and stream behavior belongs to the [protocol reference](subsystems/protocols.md).

The daemon observes host termination independently of transport and signal lifecycles; an unexpected registry exit cancels admission instead of leaving a healthy-looking socket. If durable token rotation commits but token-file publication fails, WebSocket initiators receive the stable 1012 `code=daemon_restarting` close when delivery remains possible, and all transports are then stopped for startup reconciliation.

`process_fixed` preflight never maps candidate code or mutates installed state. It reports `restart_required` while the old daemon remains available; an external operator or supervisor owns explicit stop, offline install, and fresh start. A process refuses to replace a different process-fixed artifact that it already mapped. Post-persistence publication failures and monitor inconsistencies terminate rather than continue with split routing and durable state.
