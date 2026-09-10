---
name: Owner-submitted immutable Profile inputs
---

## Problem

Controlled Profile reload resolves against its original catalog, so newly
staged executable identities cannot enter a running presentation subtree.
Manual reload also performs convergence in the caller's future, permitting a
dropped waiter to abandon the controller between membership changes.

## Decision

Keep each Host and resolver immutable. Give the composition owner a separate
input updater backed by one bounded Profile-owned command worker. Preserve the
Runtime, Context namespace, limits and nominal Local/event mappings. Retain the
old complete input for compensation and advance an independent input revision
even when a new catalog leaves the selected graph unchanged.

This extends the immutable catalog rule in
[resolved Host catalogs](../../implemented/architecture/2026-09-09-host-resolved-catalog.md)
without admitting post-build registry mutation. It preserves the binding and
compensation rules in
[Profile binding convergence](../../implemented/architecture/2026-09-08-profile-binding-convergence.md).

## Alternatives considered

A mutable shared catalog would change the meaning of an existing input and
could resolve compensation through different marker identities. A replacement
Runtime would discard ordinary child ownership and require duplicate resources.
Caller-owned convergence cannot guarantee completion after waiter cancellation.

## Verification

Public Host/Profile tests cover replacement, unused factory changes, stale
revision, marker mismatch, restart refusal, exact old-factory compensation,
manual and owner-ticket waiter cancellation, bounded admission and retirement.
Existing Profile, Host and Application tests pass, as does compilation of Host
and Profile for wasm32-unknown-unknown. Browser compilation does not establish
a browser execution result.

## Consequences

Manual reload and owner input replacement share one executing command and one
queued command. Further submissions return `Busy`; automatic source followers
retry, while an explicit reload caller receives that admission result.

Replayable convergence remains non-atomic and compensation can fail. A command
that waits for a plugin's cooperative cleanup also delays Profile retirement;
whole-Runtime shutdown closes command admission synchronously and includes that
wait in Meta's bounded shutdown outcome. Scoped retirement continues to await
actual cleanup; timing out a whole-Runtime waiter never abandons this work.

Meta disposes children before running their parent's deferred effects. A stopped
Profile therefore releases its executable inputs and targets while retaining
redacted status and tree snapshots. Otherwise an outer owner's observation handle
can keep a native factory alive while an earlier child is waiting for Loader
quiescence. Public handle-retention and real native bootstrap tests cover this
ordering, including application and embedded service shutdown.
