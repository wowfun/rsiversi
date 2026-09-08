---
name: Persistent Profile bindings and selective convergence
---

## Problem

Candidate and rollback binding allocate fresh isolation while prefix retention
preserves old Contexts. A replacement consumer may lose its retained provider.
Suffix rebuilding also retires unrelated members.

## Proposal

First preserve exact group allocations with the existing prefix algorithm.
Then implement selective convergence using stable composition positions. The
opaque namespace is Runtime identity plus owning wrapper Fiber identity and
generation. Reload preserves it; another activation creates a new namespace.

Fresh binding keys contain namespace, group InstanceId, lane, and contract key.
Named keys replace group identity with a label shared inside the namespace.
Store each group's binding delta and derive its effective inherited map.

Keep leaves whose InstanceId, resolved factory, update mode, evaluated config,
and effective binding match. Retire changed/removed leaves in reverse order,
publish candidate order, then prepare/apply individually within released capacity.
Compensation reuses exact bindings and reports Degraded if restoration fails.

## Alternatives considered

File and fragment names are not independent Profile identities. Fresh rollback
allocations cannot restore the previous graph. An Independent mode or runtime
suffix fallback would weaken the selected exact-convergence contract.

## Acceptance criteria

Multi-leaf tests reproduce isolated replacement/rollback for Local, events, and
Portable before retention changes. Selective tests cover moves, effective
binding changes, named sharing, bounded allocation history, exact Fiber capacity,
failed compensation, and real Worker behavior.

## Risks

Dependency changes can still reconcile retained members. Convergence has visible
intermediate states. Binding allocations remain alive only while current,
candidate, or compensation snapshots require them.
