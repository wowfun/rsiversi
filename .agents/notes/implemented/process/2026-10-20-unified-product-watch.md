---
name: Ordinary launch with explicit source observation
---

## Problem

Private debug launch and per-interface development publication diverge from the
ordinary product. Existing implicit Profile and native-addon observation also
prevents an explicit watch option from defining behavior.

## Decision

Invoking applications retain connection leases on an ordinary Host build plugin.
The caller supplies the frozen scrubbed build environment; the resident Service
does not lend its credentials or first-launch tool environment to builds.
The build plugin uses the ordinary Process/Sandbox providers and joins process
settlement when the last client or Host exits.

Source-specific recipes and frontend readiness remain in the application layer.
Watch leases freeze recipe, executable, source, environment and output authority.
Changes to that authority require a new lease. Validated candidates update package
resolution and existing Profile/renderer convergence without writing launch TOML.
Non-reloadable changes report restart-required.

This decision partially supersedes
[standard development](../../implemented/process/2026-10-19-standard-product-development.md)
and [isolated frontend development](../../implemented/process/2026-09-10-isolated-frontend-development.md).

## Alternatives considered

Per-client build owners cannot deduplicate commands or reject conflicting outputs
across applications. Reusing the Host's ambient environment silently changes
toolchains and credential authority. Restarting an entire Session to simulate
renderer HMR destroys the state the existing presentation child Profile retains.

## Consequences

The [application tools contract](../../../../apps/devtools/README.md) and
[launch contract](../../../../crates/rsi/core/docs/launch.md) own the ordinary
source recipes and build commands. Debug binaries use the same homes and entry
as installed binaries; developers explicitly choose isolated homes when desired.
The old private product launcher, --from selector and per-UI installation grammar
are removed. Fixture-only launchers retain cleared environments and deterministic
providers for layout tests, with explicit resident Host teardown.

No --watch means no automatic Profile or source polling. A watch lease retains
its frozen authorization; changed commands or roots need a new lease. Failed
builds keep the last admitted package, and bounded writer-lock retries do not
turn malformed candidates into retryable failures. A statically linked change
requires a core rebuild and restart.

Frontend startup supervises the actual Vite Origin and private ready handshake,
then admits that Origin through the existing HTTP owner. EOF and bounded process
settlement close the frontend, while a UI's exit does not stop the shared Host.
Windows command wrappers preserve literal arguments through the Process owner.
Renderer-only updates preserve existing UI state; changed bootstrap bytes require
restart. Protocol exactness belongs to the generated frontend control descriptor.

Declared builds are explicitly trusted user code. Process containment and an
isolated build environment do not turn them into untrusted plugin sandboxes.
Windows has no restricted execution backend. Tests exercise real child processes,
lease deduplication, conflicting outputs, last-good publication, source aliases,
Vite readiness and final-lease cleanup on their stated platforms. Fixture success
does not establish ordinary product visuals or native macOS supervision.

Source input/output layout has one product validator shared by the launch
document and Host recipe admission. Retryable output contention is an explicit
management response and client error, independent of diagnostic wording.
