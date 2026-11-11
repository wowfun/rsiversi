---
name: Workspace display order is independent of creation cursors
---

## Problem

The original DSH Workspace client calls `workspace/insertBefore`, but the native
bridge exposes no callable method. The native registry's immutable creation
positions cannot implement moving: changing them would invalidate pagination
after deletion. Browser-local view preferences also cannot satisfy a Host RPC
whose result must survive restart and reach other observers.

## Decision

Workspace owns a shared display order alongside its registration allocation.
Moves name one identity and an optional anchor and apply under the existing
commit owner. The allocation record stores creation positions, so deleting and
re-registering the same coordinates cannot revive an old slot. Registration
identity is defined by the [Workspace protocol](../../../../crates/rsi-workspace/protocol/README.md).
Registration bodies remain single-copy. New registrations lead the order; no-op
moves do not write. Reads and replies filter by the caller's location access.

Creation-order pages and bounded identity membership keep their existing
semantics. The new display order does not replace browser-local viewing choices.
DSH's own Workspace UI keeps Session manual views locally while delegating
Workspace dragging to the Host; those distinct consumers require distinct state.

## Alternatives considered

Changing creation positions breaks durable cursors. Duplicating the registry in
the bridge creates a second owner for paths, titles and deletion. Replacing all
browser preferences with Host state is unsupported by the original DSH client.
A per-record fractional-rank scheme adds allocation and rebalance machinery to
infrequent user moves when a bounded integer list already fits the owner.

## Consequences

Each changed move writes one bounded record and publishes after durability.
Deleted positions may remain in that record until the next allocation or changed
move; opening and live deletion exclude them immediately. The retained list does
not grow without a write and never retains content or additional registrations.
The existing Storage fence handles uncertain commits; no rollback journal,
second registry, retry loop or DSH-specific native operation is introduced.
