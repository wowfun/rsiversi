---
name: Complete extension projections over captured Session state
---

## Problem

Extension consumers need consistent draft and durable values without duplicating
domain reduction in each UI. Reading only Fact progress misses idle command
changes. Preparing an execution claim for a view incorrectly couples independent
history inspection to every current domain codec and runtime dependency.

## Decision

The [Agent composition contract](../../../../crates/rsi-agent/composition-protocol/README.md)
owns pure projection units in the ordinary immutable contribution catalog. A
framework adapter captures complete domain state and drives each unit at the
same draft revision or durable Fact/control cut. Individual failure is a bounded
producer result, isolated from other extension values and core conversation
history. Callbacks receive no Store writer or command capability.

The [Kernel contract](../../../../crates/rsi-agent/kernel/README.md) owns independent
read-generation selection: existing residents retain their pin; cold views do
not hydrate execution or require unrelated codecs. Concurrent resident publication
takes precedence even after failed cold resolution. Store pages must match the
requested horizon in addition to satisfying their own structural invariants.

The [Session contract](../../../../crates/rsi/session-protocol/README.md) owns complete
subscriptions and final-clone canonical byte retention. Draft state is captured
under its mutation admission; callbacks run afterward. Scoped commit hints and
draft notifications subscribe before capture. Idle subscriptions retain no draft
activity or separate generation pin. A changed Header ends the old stream; a new
subscription rebinds instead of reinterpreting old results against new identity.
The [API contract](../../../../crates/rsi/session-api/README.md) independently bounds
and validates envelopes, decoded snapshot identity and both durable watermarks.

## Alternatives considered

Client-side domain folding duplicates semantics and makes a missing prefix an
extension-specific recovery problem. Fact-only invalidation misses command
commits. Reusing resume preparation makes an unrelated missing codec hide all
views. Retaining a lease for an idle observer prevents draft expiry. Reusing the
old stream binding after preset selection admits mixed Header identity.

## Consequences

These views are disposable and introduce no durable format or projection cache.
Bounds charge canonical payload ownership rather than arbitrary native plugin
allocation or allocator RSS. Cold projection may retain independent readable
values while executable cold recovery correctly refuses incomplete codecs.
The DSH framework-driven complete-value projection informed this boundary;
RSI additionally needs independent Fact/control progress and unpublished drafts.
Shared conversation semantics and interactive render contributions remain separate
consumers, not a new authority for extension state.
