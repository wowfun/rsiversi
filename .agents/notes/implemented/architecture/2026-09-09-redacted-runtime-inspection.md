---
name: Operational inspection reads existing ownership without exporting plugin state
---

## Problem

A workbench needs to explain factory provenance, dependencies, contribution order
and retained effects. Existing lifecycle snapshots contain plugin-provided failure
strings and omit the relationships that explain those states. Reconstructing a
second registry in product code would drift from actual generation ownership.

## Decision

Meta reads its existing Fiber, prepared-requirement, binding, supply, effect and
composition-position records into bounded owned metadata pages. It emits lifecycle
kinds and a terminal boolean without raw diagnostics, configuration, service values,
opaque state or effect labels. Prefix collections report their complete counts.
Membership pagination uses stable Fiber IDs; actual contribution order is a separate
captured composition path.

Whole-Runtime inspection can include the existing logical resource ledger. Scoped
Context inspection includes only its owning generation's subtree and omits global
resource counts. The owning generation is checked before and after observation.
Registry membership capture, Fiber data reads, composition order and effect reads
use their respective locks without nesting effect or order locks under Fiber locks.
No plugin callback runs as part of inspection.

Cleanup transfers effect records and children out of active generation tables.
Inspection preserves that distinction: table prefixes do not claim to enumerate
executing cleanup. Existing generation effect budgets and the cleanup phase show
retained work after transfer, without creating a second effect registry or holding
extra strong cleanup owners. Supply publication metadata names the generation's
publication history rather than promising current call admission.

RunningHost forwards whole-Runtime observation without exporting mutable Runtime
access. ScopedProfile uses its existing real scope Context; RunningRsi combines
that observation with its existing Profile control status and desired snapshot.
Two embedded Hosts therefore retain separate inspection membership and neither
receives the shared Runtime's global resource counters through this product API.

## Alternatives considered

Serializing the existing RuntimeSnapshot would expose arbitrary diagnostic strings
while still missing dependencies and effects. A product-side mirror would need to
interpret lifecycle events and compensate races already owned by Meta. Holding all
locks for a supposedly atomic graph would expand lock coupling into operational
inspection and interfere with teardown.

## Consequences

These pages explain bounded current observations; they are neither an atomic graph
nor proof that foreign callbacks or cleanup have quiesced. A collection prefix can
omit details while its total remains visible. Callers own returned page retention.
Identifiers and provenance are metadata, so plugins must not use them as secret
storage. Read-only inspection creates no service or mutation authority, although
trusted safe-Rust callers may already possess other explicit Runtime capabilities.
