---
name: Portable Tool contributions use the existing unpublished catalog
---

## Problem

Native ABI plugins publish Portable services, while Tool orchestration consumes
an immutable Local catalog. A separate native registry would duplicate policy,
admission, retained outcomes and generation ownership.

## Decision

One ordinary linked contributor imports an explicitly injected Portable supply
into the exact Local ToolRegistrar. The native side describes one bounded batch;
the existing stage registers atomically, and its existing seal and result owner
remain authoritative. The standard Agent catalog makes the bridge available but
does not enable it without an explicit Profile leaf.

The wire belongs to [Tools protocol](../../../../crates/rsi-tools/protocol/README.md#portable-contributions).
Execution carries the already-resolved call and policy. Duplex Confine requests
omit mode, cwd and workspace overrides, so the host uses the original ToolExecution
Sandbox and enforcement collector. Process plans preserve native OS strings.
Native results cannot assert enforcement stamps. Both endpoints use the bounded
canonical Tool JSON decoder; the empty Describe variant is a struct because
Serde internally tagged unit variants accept extra fields even with the enum's
deny_unknown_fields annotation.

## Alternatives considered

Invoking a native tool directly from the Agent executor bypasses the catalog's
policy and settlement boundary. A new native Tool registry creates competing
ownership. Serializing the entire ToolExecution would leak arbitrary local
capabilities and weaken its Sandbox boundary. Treating native-reported process
stamps as host evidence would falsely attest enforcement.

## Consequences

Native plugins remain trusted in-process code; requesting a confined plan does
not sandbox the plugin or prove that it spawned that plan. Actual stamps are
collected by the same host planner used by linked tools. Other invocation
capabilities, such as Jobs and arbitrary extensions, are not serialized.

The sealed catalog cannot override Meta capability-generation fences. Product
pins must retain the native provider and bridge with the Tool catalog, and use
fresh isolated generations for replacement. Failure of a native callback may
retain Loader resources beyond a Tool error; the bridge makes no foreign-thread
quiescence or forced-unload claim.

Public tests cover atomic registration/rollback, invalid descriptions, framing,
forged enforcement, wrong phase, missing/extra results and cancellation. A real
ABI v3 fixture executes Tool Describe, Execute and host Confine through NativeCatalog
and this same bridge, with clean release after owners retire. Its evidence does
not establish actual process execution, failed finalization, UI or live-provider
behavior for native addons.
