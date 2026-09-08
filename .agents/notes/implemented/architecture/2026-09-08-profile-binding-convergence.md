---
name: Persistent Profile bindings and selective convergence
---

## Problem

Candidate and rollback binding allocate fresh isolation while prefix retention
preserves old Contexts. A replacement consumer may lose its retained provider.
Suffix rebuilding also retires unrelated members.

## Decision

Profile preserves exact group allocations and converges by InstanceId using
stable composition positions. The
opaque namespace is Runtime identity plus owning wrapper Fiber identity and
generation. Reload preserves it; another activation creates a new namespace.

Fresh binding keys contain namespace, group InstanceId, lane, and contract key.
Named keys replace group identity with a label shared inside the namespace.
Each group stores its own binding delta and derives its effective inherited map.
An immutable Arc per declared isolation lane avoids cloning identifier strings
for every flattened descendant or candidate copy. The namespace lookup keeps
weak allocation references and prunes dead entries when binding. Current,
candidate and compensation snapshots alone retain live allocations.

Convergence keeps leaves whose InstanceId, resolved factory, update mode, evaluated config,
and effective binding match. It retires changed/removed leaves in reverse order,
publishes candidate order, then prepares/applies individually within released
capacity. Pure reorder also retains RestartRequired members; an actual config,
factory or effective binding change still respects that mode.
Compensation reuses exact bindings and reports Degraded if restoration fails.

## Alternatives considered

File and fragment names are not independent Profile identities. Fresh rollback
allocations cannot restore the previous graph. An Independent mode or runtime
suffix fallback would weaken the selected exact-convergence contract.

## Consequences

Public multi-leaf tests cover isolated replacement/rollback for Local, events,
and Portable. Selective tests cover moves, effective
binding changes, named sharing, bounded allocation history, exact Fiber capacity,
failed compensation, and real Worker behavior. The same actual-provider namespace scenario runs in
Linux and Chromium/Firefox Workers; mutable file-driven reload is verified in
Linux, while both targets verify the underlying order and replacement primitives.

Dependency changes can still reconcile retained members. Convergence has visible
intermediate states. Binding allocations remain alive only while current,
candidate, or compensation snapshots require them.
