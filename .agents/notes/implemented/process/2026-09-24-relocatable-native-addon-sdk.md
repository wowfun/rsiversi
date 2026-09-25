---
name: Relocatable native addon SDK distribution
---

## Problem

Native generated projects currently depend on absolute paths into the generator
checkout. The older linked SDK pin also speaks Portable version 1 while the
current Host accepts version 2, so copying its revision cannot prove loadability.

## Decision

Partially supersedes the [native scaffold decision](2026-09-17-native-addon-scaffolding.md)
by pinning both templates to published revision
`386858d6f266fdf8d5c58e19793e5d2d09d3da34`. Keep immutable independent locks and
atomic offline generation. Verify actual native Describe/Execute in the current
Host, separately from the current-tree SDK tested through explicit temporary
dependency overrides. Refresh the linked pin and lock in the same change.

## Alternatives considered

Retaining absolute SDK paths preserves development convenience at the cost of
relocation. A new unpublished commit would require publication authority and
would not establish that users can fetch it. The already published revision has
the required wire version without committing this dirty checkout.

## Consequences

Generated manifests contain no SDK paths, survive relocation and build offline
with a populated controlled cache. The current Host loads and executes the
published artifact. Linked lifecycle tests pass with the new pin. Current-tree
tests and published-revision tests have distinct artifacts and reports.

The published revision cannot prove uncommitted SDK changes. A future Portable
wire change requires another compatible published pin before distribution.
First fetch needs network access; generation itself remains offline.

Linux CI runs both current-tree overrides and the unchanged published native
template lock through the scaffold/relocation/Loader scenario. The latter first
fetches its exact pinned graph; a regenerated override lock cannot establish
distribution freshness.
