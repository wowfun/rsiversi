---
name: Bounded Session metrics from validated attempt usage
---

## Problem

Transcript windows cannot establish complete Session totals, and ambiguous cache
aliases cannot establish a provider-independent token or cost denominator.

## Decision

TokenUsage owns inclusive input/output totals and validated cache/reasoning
subsets. Adapters normalize according to explicit wire accounting. A pure
conversation reducer folds effect-tagged usage at a fixed durable cut; Session
owns bounded forward replay and discardable caches. Usage reported by a failed
attempt still counts. Own-Session totals exclude inherited history and children;
task-tree aggregation is an explicit detail read with completeness markers.

Context pressure uses only the last successful Conversation request and its
captured model capacity/output reserve. It is labeled as a previous request,
invalidated by relevant changes, and never presented as a current estimate.

The current selection is a read-time Session value, separate from the metrics
watermark and durable projection cursor. Its router description binds committed
generation, endpoint/protocol and profile. Context comparison requires the exact
description captured by the previous successful request; checking a numeric
generation alone is insufficient across Host restarts. Selection/profile reads
do not add invocation authority or write an otherwise unchanged domain revision.

## Alternatives considered

Summing visible cards undercounts evicted history. A universal alias precedence
can conflate inclusive and exclusive provider counters. Session-lifetime effect
sets or full-history Todo folds create unbounded retained state.

## Consequences

Tests cover validated counters, duplicate/retried effects, Usage plus Failed, cold replay,
bounded caches, isolated parent/child totals and stale context indicators.


Cold totals may take multiple bounded slices. Unknown breakdowns remain unknown
and partial totals cannot be labeled complete. New TokenUsage validation can
reject previously accepted malformed provider events.

Tree membership is captured once per explicit scan, while each member has its
own fixed Fact watermark. These are not an atomic simultaneous tree snapshot.
Store pages remain indivisible: a valid Fact exceeding the 16 MiB processed
slice runs alone under the Store page bound. This guarantees forward progress
without inventing a second storage projection API.

The current owning contract and implementation are in the [owning package](../../../../crates/rsi/session/README.md).
