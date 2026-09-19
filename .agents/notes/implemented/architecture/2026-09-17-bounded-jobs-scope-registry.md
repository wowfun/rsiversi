---
name: Bounded current Jobs scope mappings
---

## Problem

Empty scopes consume registry entries independently of active and retained job
limits. Sweeping every acquisition scans the map under the shared Jobs mutex,
including ordinary lookups of an existing scope.

## Decision

The [local Jobs contract](../../../../crates/rsi-jobs/local/README.md) bounds
current mappings and checks an active matching scope before cleanup. An insertion
only sweeps weak mappings when the current map is full. Exact-key replacement
removes its stale mapping directly. Weak entries below capacity do not retain
authority and need no periodic sweep under the shared mutex.
Shutdown admission is checked again while holding the registry mutex.

## Alternatives considered

A strong provider registry would extend authority lifetimes. Drop callbacks or
a secondary dead-key queue would add lifecycle coordination to an existing
weak registry. A periodic insertion budget, even one proportional to backing
capacity, adds scans below the map limit without releasing live resources.
Capacity-only cleanup removes that work and its scheduling counters; it does
not remove the bounded scan needed to reclaim a dead entry in a full map.

## Consequences

An empty but retained current scope can cause explicit Capacity backpressure.
The limit counts current mappings, not historical authority handles retained
by callers. Ordinary active lookups do no sweep; acquisitions at saturation can
still pay one full sweep. Exact generations and provider retirement remain the
authority checks. Tests require zero sweep work below the limit and count backing
capacity work under saturation and after map shrinkage, rather than asserting
machine-dependent timing.
