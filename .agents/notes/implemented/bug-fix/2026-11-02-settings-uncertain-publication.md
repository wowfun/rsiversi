---
name: Recover visible Settings state after uncertain publication
---

## Problem

A final directory sync can fail after the replacement document becomes visible.
Treating that failure as an ordinary rejected write leaves the registry serving
an old value and revision while later CAS observes the new raw section.

## Decision

The [Settings registry](../../../../crates/rsi-settings/core/README.md) owns
namespace reconciliation. It retains write ownership, reports the original
outcome as unknown, and makes one bounded readback with the namespace validator.
A successful observation advances the revision, including when the observed
value is unchanged. Unresolved namespaces reject ordinary access until an
explicit asynchronous read recovers them. Markers survive lease retirement.
The local provider's blocking worker owns its serialization gate.

## Alternatives considered

Restarting the whole Host or permanently fencing Settings would make unrelated
namespaces unavailable. Repeating a sync and declaring success would not prove
that the original publication became durable. Replaying a mutation could apply
user intent twice. Readback restores a usable visible-state authority without
making any of those claims.

## Consequences

Wire version 4 adds a distinct recovery failure. Callers preserve an unknown
mutation receipt while allowing explicit refresh. Observation does not prove
power-loss durability. A failed read or invalid section can keep that namespace
unavailable until the provider state is repaired; other namespaces remain usable.
