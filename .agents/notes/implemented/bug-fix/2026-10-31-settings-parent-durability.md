---
name: Publish Settings directory ancestors before acknowledging a write
---

## Problem

Unix Settings writes sync their temporary file and final parent directory, but
recursive directory creation can leave earlier parent names unflushed. A
successful replacement alone therefore does not establish the durability of a
new directory chain. Storage already synchronizes all ancestors, including ones
left by an interrupted attempt.

## Decision

The native filesystem library owns Unix `sync_directory_ancestors`. Settings
calls it after private directory creation and before writer-lock acquisition;
Storage delegates its existing ancestor synchronization to the same helper.
Every selected ancestor is synchronized, and the first failure prevents success.
This operation retains the callers' path-based directory-alias policy and does
not acquire no-follow directory authority. Windows publication keeps its existing
retained-parent implementation.

## Alternatives considered

Syncing only the final directory does not publish its ancestors' entries.
Syncing only newly created directories can miss a prefix left by an interrupted
attempt. Replacing creation with the no-follow helper would also change path
admission and still leave pre-existing ancestors outside that helper's contract.
Duplicating Storage's loop inside Settings would give the same durability rule
two implementation owners.

## Consequences

Unix writes perform directory synchronization before publishing Settings. Errors
propagate while existing directory permissions and Settings revision CAS remain
unchanged.
The complete chain must be readable and synchronizable. An execute-only ancestor
can therefore reject a write that ordinary pathname traversal would allow;
the provider does not alter that directory's permissions or silently omit its
durability barrier. The per-write cost grows with directory depth. Avoiding it
requires a separate proof that every existing ancestor was already published,
including a prefix left by an interrupted attempt.
The moved ancestor-order/failure test and a public nested Settings
write test cover the contract; Linux syscall traces check the actual calls.
These checks establish ordering, not a physical power-loss guarantee or native
macOS validation.
