---
name: Generation-pinned Agent contributions and durable domain state
---

## Problem

The durable domain substrate and ordinary execution contributions need command,
projection and draft consumers before interactive business features can use
the same extension paths as independent addons.

## Proposal

The [model-context builder decision](../../implemented/architecture/2026-09-08-model-context-builder.md)
owns context selection, exact execution/maintenance pinning and cache envelopes.
The [typed domain decision](../../implemented/architecture/2026-09-08-typed-domain-commits.md)
owns complete state, exact-generation proposals, revision CAS, canonical
receipts, fresh/fork baselines, mixed generated-record budgets and constrained
ending. This proposal begins with consumers of that substrate.

The [terminal boundary decision](../../implemented/architecture/2026-09-08-terminal-control-boundaries.md)
owns same-transaction Fact/control correlation, exact historical fork prefixes,
control-admission fences and partial startup repair. Domain as-of selection uses
that established control horizon. The
[execution contribution decision](../../implemented/architecture/2026-09-08-execution-contributions.md)
owns PluginContext, ToolRejected, stable ordering, bounded execution stages and
workspace/time migration. The
[Session command decision](../../implemented/architecture/2026-09-09-session-command-admission.md)
owns command dispatch, draft values, preset selection and transport binding.
This proposal continues with projections and planning policy, consuming those
established interfaces.

## Alternatives considered

Arbitrary JSON writers bypass domain validation; separate Storage creates dual
authority. Sampling time inside a builder loses provenance. Cache-envelope
compatibility cannot replace an explicit authoritative Store schema cutover.

## Acceptance criteria

Builder and substrate evidence precedes contribution execution. Tests cover
persisted actual input, retry reuse, policy rejection, bounded callbacks, draft
commands and receipts, projections, cold recovery and zero replayed effects.
Authoritative formats use explicit schema cutovers with old databases preserved.

## Risks

Callbacks must not bypass the existing correlated terminal, budget or mutation
admission. Missing cold-generation contributions must not replay external work;
extension projection failures must not hide core history.
