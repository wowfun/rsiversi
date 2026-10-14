---
name: Windows compilation with explicit unsupported authority
---

## Problem

Windows compilation stopped at Automation's Unix filesystem helpers and a
missing Desktop ICO, hiding unguarded Unix Profile-management references and a
Profile-owner match arm. Removing only the missing calls would expose
Automation paths whose privacy checks compile away on non-Unix hosts.

## Decision

[Automation](../../../../crates/rsi/automation/README.md) refuses non-Unix
preparation and acquisition before filesystem effects. Its private-file boundary
also rejects. The product MCP-management factory refuses without its Unix
Profile authority, and the Unix fixture client refuses before reading input or
owner metadata. Platform-specific helper declarations match the platforms of
their existing consumers; pure decoder tests remain cross-platform.
Desktop embeds a real multi-resolution icon. Compilation is a prerequisite,
not an application-support receipt.

The [JSON Storage](../../../../crates/rsi-storage/json/README.md),
[SQLite Storage](../../../../crates/rsi-storage/sqlite/README.md) and
[local Settings](../../../../crates/rsi-settings/local/README.md) and
[Media](../../../../crates/rsi-media/local/README.md) owners also
reject non-Unix acquisition before filesystem effects. Their previous
non-Unix successful no-op privacy or sync helpers do not establish those
contracts.

Public capability-family errors distinguish unsupported provider/platform
admission from invalid caller input. Storage, Settings, Media and Agent Store
retain their own typed `Unsupported` category; their API adapters preserve the
owning failure code or operation unavailability. Fixing an argument cannot
supply a missing native protection mechanism, and this refusal is never an
uncertain durable mutation.
At each factory preparation seam, malformed configuration remains
`MetaError::InvalidInput`; an unsupported capability uses `MetaError::Activation`,
whose foundation contract includes preparation failures. It does not require a
new foundation platform-policy variant. Each capability owner retains its own
admission predicate and error type rather than delegating policy to native-fs.

Agent Store acquisition, offline operations and explicit reset likewise reject
before accessing or moving old data. Internal CAS opens and directory inspection
also refuse there; retaining an unreachable path-based backend would not establish
the missing private root or SQLite handle validation. This narrows the
Windows admission described by the [Store validation decision](../architecture/2026-09-01-agent-store-validation-and-read-snapshots.md)
while preserving its Unix durability and read-snapshot decisions.

The [product preset cache](../../../../crates/rsi/core/README.md) likewise
refuses non-Unix materialization before accessing old or new assets. Its removed
path-based fallback published System sources after successful no-op permission
checks. The [terminal](../../../../apps/terminal/README.md) refuses non-Unix
stdin acquisition and signal admission instead of using an uncancellable reader
or a permanently pending signal future. Directory-picker refusals use ready
futures while retaining their existing authorization checks.
[Serve](../../../../apps/serve/README.md) preparation rejects before planning
an HTTP child; a Ctrl-C-only waiter cannot establish its native signal lifecycle.

The shared HTTP access gate retains its fallible signature: Unix
Local callers are rejected there even though Windows currently has only the
Remote variant. A narrow platform-scoped lint expectation documents that
exhaustive enum difference; it changes no authentication check.

The current Linux contracts in the [Service decision](../architecture/2026-09-01-local-service-host.md),
[terminal decision](../feature/2026-09-06-terminal-application.md),
[Credentials decision](../architecture/2026-09-14-file-credentials.md) and
[Profile decision](../architecture/2026-09-09-profile-edit-preview.md) remain
authoritative until their native Windows mechanisms pass acceptance.

## Alternatives considered

Path-based directory creation with skipped privacy checks would compile while
weakening the owning boundary. Removing the whole Automation public interface
would break consumers that need an explicit refusal. Exposing the Unix
Profile-management implementation on Windows would substitute path checks for
the source authority it requires. None supplies the missing protection.

## Consequences

The Windows gate executes unfiltered native Process, Files, Credentials and ACP
application tests alongside the private-store and client targets. Compilation
and lint checks cannot substitute for their Windows-only behavior tests.

Windows builds can expose subsequent compiler errors without granting unsupported
authority. Native refusal tests assert zero new entries for absent and existing
isolated directories. The wider native implementation remains gated on actual
feasibility. A standard-user Windows 11 probe of Low-integrity write-restricted
tokens found Node pipe-child creation denied in the token shapes intended for
ReadOnly and WorkspaceWrite, including the Browser worker's CDP stdio shape.
The current [worker](../../../../crates/rsi/browser/runtime/worker.mjs) requires
those pipes; a Windows Browser support claim requires a different verified
launch/transport design. This is not evidence that Chromium's own sandbox failed
to initialize: the process-creation boundary fails first.

The probe was independently written against Win32, using the DSH token shape
and test scenario as evidence: deepseek-harness commit
`d743267388641bc76f17c45ce8b4c231aed1d32c`,
`packages/sandbox/sandbox-windows-acl/src/token.ts` and
`tests/runner.spec.ts`. It uses a kill-on-close job without breakaway flags,
explicit inherited handles and token inspection. These observations cover that
probe on Windows build 26200 and Node 26.5.0, not every toolchain or native runner.
