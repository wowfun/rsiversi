---
name: Separate Workspace registration identity from coordinate keys
---

## Problem

Native WorkspaceId was a hash of execution coordinates. Deleting and registering
the same path reused both its ID and its Navigation account. Real Windows and WSL
DSH RPCs reproduced this. DSH assigns a fresh UUID at registration, and its client
retains deleted IDs to reject delayed upserts. Clearing just the old account would
leave the reused client identity unusable.

## Decision

Give each native registration an opaque random ID and a separate indexed
coordinate key. Existing record storage owns the ID; the coordinate index is
derived in memory. Current contracts belong to the
[Workspace protocol](../../../../crates/rsi-workspace/protocol/README.md).
History and review retain their coordinate scope using WorkspaceKey. Clients must
resolve a current registration when they need a registration ID.

## Alternatives considered

A deletion hook clearing Navigation state would not repair DSH client tombstones.
A JS ID translation table would duplicate native registry lifetime and state.
Encoding the allocation sequence into IDs would couple identities to allocator
reset and expose its internal representation. Random IDs use the existing ID
field, with no extra persisted counter or generation field.

## Consequences

Registration reads and mutations cannot accidentally target a replacement.
Deleting a registration does not delete directories, Sessions or history caches.
Coordinate lookup remains exact and indexed, including for unregistered paths.
The wire contract changes because get and find now have distinct semantics.
