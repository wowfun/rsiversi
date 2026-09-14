---
name: Persistent file credentials for application login
---

## Problem

Mandatory OS keyring availability prevents persistent TUI login in ordinary
headless Linux environments. Environment bindings also block replacement even
when the saved credential takes precedence. This supersedes only the keyring
storage decision in [terminal setup](../feature/2026-09-13-terminal-setup.md).

## Decision

The standard product uses one private file backend selected by standard Host composition and owned by
the credentials family. Saved entries precede captured environment values;
only missing entries permit fallback. File failures remain visible. It retains
Resolve/Admin separation, bounded blocking admission and independent mutation
receipts. Historical Keyring provenance remains readable without a keyring backend.
The [local contract](../../../../crates/rsi-credentials/local/README.md) owns the
format, file trust checks, publication and failure rules.

## Alternatives considered

Memory-only login does not survive Host restart. A keyring-first fallback creates
host-dependent persistence behavior. Keeping an optional keyring adds another
configuration and maintenance path. The user selected file-only persistence and
manual re-login instead of importing existing secrets. Pi demonstrates a usable
file-first login flow; its in-place writes are not the chosen publication model.

## Consequences

Real file and API tests exercise replacement, precedence, bounded decoding,
concurrent processes, dropped response waiters and uncertain publication.
Mutation completion invalidates earlier singleflight lookups; older cleanup
cannot remove a newer lookup. Sized zeroizing buffers avoid clearing the maximum
file budget for each small operation.

Linux PTYs complete login, chat and restart against embedded and daemon Hosts
without API-key environment values. Linked/native visual captures cover three
terminal sizes. Opt-in DeepSeek runs verify real responses before and after Host
restart and leave no temporary credentials. Historical failure logs and final
artifact hashes remain in local evidence; they are not product contracts.

Files are unencrypted and accessible to processes running as the same OS user.
Unix permissions protect against other ordinary users, not a process sandbox.
Non-Unix platforms currently lack equivalent implemented private-file mechanics
and report Unsupported. Windows/macOS cross-compilation and Web WASM compilation
passed; native Windows/macOS execution is not established. Native Windows TUI
support remains outside this change.
