---
name: Native Workspace Session accounts retain manual order
---

## Problem

DSH's WorkspaceEntity attaches new Sessions at the front and changes that order
only through insertSessionBefore. Its activity index is a separate projection.
The companion instead sorted every baseline by activity and did not implement
the move RPC, so ordinary activity discarded the intended account semantics.

## Decision

Navigation owns the shared order alongside its existing title, pin and archive
metadata. Workspace remains a generic coordinate registry without Agent imports;
Session Store remains the only source of membership and immutable creation time.
An order contains identity references only and is written on explicit movement.
Reading a baseline neither persists a second membership registry nor runs a
background synchronization task. New members precede the saved subsequence;
missing members are omitted and pruned on a later mutation or explicit purge.

The existing serialized Navigation writer commits order and revision together.
The current contract lives in the [Navigation README](../../../../crates/rsi/navigation/README.md). Browser-local manual view
preferences remain separate from this shared Workspace account.

## Alternatives considered

A second JS account registry would duplicate membership and add reconciliation
and process lifetime dependencies. Persisting every default order would create
writes merely by viewing a Workspace. Putting Session identities in the generic
Workspace registry would reverse its dependency direction. The existing native
Navigation writer already owns shared Session metadata and its commit boundary.

## Consequences

Explicit movement retains only identity references, while absent order state has
the direct meaning of no explicit movement. The existing single-document format
still entails cloning and encoding metadata on a real move; this change is not
a resource benchmark or a claim of per-Workspace storage transactions.
Account identity currently follows coordinates. Registration deletion/recreation
and late import with historical creation timestamps need separate parity work;
the new move path alone does not establish complete DSH account equivalence.
