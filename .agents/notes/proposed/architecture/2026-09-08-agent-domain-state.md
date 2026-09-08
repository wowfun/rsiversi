---
name: Generation-pinned Agent contributions and durable domain state
---

## Problem

Workspace context has dedicated Kernel branches. Fact-only budgets cannot account
for plugin control records, and Session controls have no typed domain-state admission.

## Proposal

The [model-context builder decision](../../implemented/architecture/2026-09-08-model-context-builder.md)
owns context selection, exact execution/maintenance pinning and cache envelopes.
This proposal begins at the authoritative durable-state boundary.

Introduce typed complete domain state in Session controls. Kernel admits
validated proposals, revision CAS, and receipts. GeneratedRecords/Bytes charge
Facts and Turn-attributed domain controls; user commands and baselines use
separate bounded admission. Plugins cannot select their charging class.

The [terminal boundary decision](../../implemented/architecture/2026-09-08-terminal-control-boundaries.md)
owns same-transaction Fact/control correlation, exact historical fork prefixes,
control-admission fences and partial startup repair. Domain as-of selection uses
that established control horizon. PluginContext preserves actual model input;
ToolRejected records denial without a fake start. Initial domain baselines join
Header and first acceptance atomically.

Only after substrate tests pass, migrate workspace/time contributors, followed
by commands, projections, draft controls, and planning policy. Ordered consumers
use stable composition positions.

## Alternatives considered

Arbitrary JSON writers bypass domain validation; separate Storage creates dual
authority. Sampling time inside a builder loses provenance. Cache-envelope
compatibility cannot replace an explicit authoritative Store schema cutover.

## Acceptance criteria

Builder equivalence precedes durable changes. Tests cover mixed budgets, atomic
failure, terminal correlation including recovery, historical forks, draft
receipts, unknown codecs, and zero replayed effects. Authoritative formats use
explicit schema cutovers with old databases preserved.

## Risks

All terminal producers must use one correlated commit. Required cleanup must
remain possible after work budgets expire. Missing cold-generation codecs block
execution while history remains readable.
