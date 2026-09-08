---
name: Generation-pinned Agent contributions and durable domain state
---

## Problem

Execution/checkpoint construct ContextFold directly. Workspace context has
dedicated Kernel branches. Fact-only budgets and fork boundaries cannot account
for plugin control records or establish historical domain state.

## Proposal

First extract an Agent-owned ModelContextBuilder wrapping ContextFold; execution
and checkpoint retain the same pin. Context-owned cache envelopes bind builder,
configuration, limits, Header, and Fact prefix. Store keeps one bounded slot.

Then introduce typed complete domain state in Session controls. Kernel admits
validated proposals, revision CAS, and receipts. GeneratedRecords/Bytes charge
Facts and Turn-attributed domain controls; user commands and baselines use
separate bounded admission. Plugins cannot select their charging class.

TurnBoundaryRecorded accompanies each terminal Fact in one atomic commit and
defines its control horizon. Forks retain both prefixes. PluginContext preserves
actual model input; ToolRejected records denial without a fake start. Initial
domain baselines join Header and first acceptance atomically.

Only after substrate tests pass, migrate workspace/time contributors, followed
by commands, projections, draft controls, and planning policy. Ordered consumers
use stable composition positions.

## Alternatives considered

Arbitrary JSON writers bypass domain validation; separate Storage creates dual
authority. Sampling time inside a builder loses provenance. ContextFold already
rejects different-limit checkpoints, so a new Store key is not a safety repair.

## Acceptance criteria

Builder equivalence precedes durable changes. Tests cover mixed budgets, atomic
failure, terminal correlation including recovery, historical forks, draft
receipts, unknown codecs, and zero replayed effects. Authoritative formats use
explicit schema cutovers with old databases preserved.

## Risks

All terminal producers must use one correlated commit. Required cleanup must
remain possible after work budgets expire. Missing cold-generation codecs block
execution while history remains readable.
