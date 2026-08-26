---
name: Runtime-owned plugin foundation
comment: Minimal Context and Fiber core with adapters as ordinary plugins
---

## Problem

The former `rsi-meta` platform combined composition, persistence, recovery,
native loading, process protocols, and product integrations. Those layers
duplicated lifecycle authority and made an ordinary plugin impossible to reason
about independently from a privileged host.

The repository still needs one durable decision that owns the minimal runtime
boundary even though the original static-declaration and native-v1 details have
been replaced.

## Decision

`Runtime -> Context -> Fiber` is the sole composition and lifetime hierarchy. A
Runtime owns bounded resources and convergence. Immutable Context values carry
derived local authority. Applying a plugin creates a separately managed Fiber
whose generation owns setup, calls, contributions, and cleanup.

Core contains no package manager, daemon, persistence engine, global product
registry, watcher, or privileged native host. Products add those policies as
plugins or callers. Execution backends and discovery systems adapt into the
same `PluginFactory` and Fiber lifecycle rather than creating another owner.

The current dependency, effect, capability, scope, and native contracts are
owned by the
[Cordis capability foundation](2026-08-24-cordis-capability-foundation.md).
The archived original form of this decision records the superseded static
declaration and ABI-v1 design.

## Alternatives considered

A central composition platform is rejected because persistence, transport,
discovery, and product policy have different trust and failure boundaries. A
composition-only abstraction with no Runtime ownership is also rejected because
cleanup, shutdown, resource admission, and generation fencing require one
authoritative lifecycle.

## Consequences

Every durable authority has an explicit Runtime/Fiber owner or is an
effect-owned contribution to one. Adapter-specific unsafe code and external I/O
stay outside safe core. Product features cannot rely on hidden platform
services; they must expose their own typed plugin or caller boundary.

This note intentionally owns only the stable minimal hierarchy. It does not
duplicate the current detailed capability foundation or preserve superseded
wire and declaration behavior.
