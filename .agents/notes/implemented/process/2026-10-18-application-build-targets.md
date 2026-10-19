---
name: Explicit application build targets and isolated publication caches
---

## Problem

The source entry builds Web even for terminal-only servers. Hidden subprocess
output obscures long compilation, and one current record cannot retain separate
TUI, Web and Desktop selections. Windows Desktop cannot be published despite its
native adapter and resource support.

## Decision

The [application tools contract](../../../../apps/devtools/README.md) owns stage
selection, progress, per-target publication and explicit same-bundle launch.
Default TUI preserves the full native catalog without requiring frontend tools.
Its Linux SSH helper is opt-in; Web/Desktop retain their helper default. Strict
family identity remains, with separate stable captures and Cargo caches.
Windows Desktop uses the same paired producer and checks WebView2 at launch.

This partially supersedes the shared capture/default Web choices in
[paired Web publication](../architecture/2026-09-26-application-ownership-and-paired-web.md)
and [source onboarding](../architecture/2026-10-16-source-onboarding-and-windows-applications.md).
The [SSH lifecycle](../feature/2026-09-29-ssh-execution-and-helper-lifecycle.md)
keeps its verified upload and generation ownership; only optional packaging changes.

## Alternatives considered

A Cargo Web feature removes no frontend tool requirement that stage selection
cannot remove, but changes catalog identities and saved Profile availability.
It is rejected. A normalized cross-target family would weaken or redesign the
existing producer contract. Independent caches instead trade disk and first-build
cost for predictable repeated builds. Silent compatible-bundle fallback would
make path selection and Host identity surprising; explicit --from selects it.
The official DeepSeek harness supplies stage/timing diagnostics and separately
packages its SSH helper. Its broad default build and the unofficial harness's
workspace/all-features build are not adopted.

## Consequences

Target-specific captures preserve strict family identity and allow repeated builds
to reuse the same target's manifest and compilation cache. Shell/publication tests
cover prerequisite-free help, parameter boundaries, target selection, isolated GC,
optional helper packaging and diagnostic streams. Paired fixture builds replay the
immutable Web manifest and reject a changed capture. Runtime checks retain the
existing asset and helper hashes; Desktop checks WebView2 before creating state.

Separate captures and Cargo caches use more disk and have separate initial build
costs. Different target families cannot share a Host unless clients explicitly
select the same publication. Legacy cache directories are not launch fallbacks
and remain for deliberate offline cleanup. System runtimes are not bundled.
