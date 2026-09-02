---
name: Tool settlement owns invocation admission
comment: Catalog withdrawal cannot release capacity still used by active code
---

## Problem

Tool capacity was counted from queryable retained entries. Dropping a catalog
removed Pending entries even when non-cooperative Tool bodies still ran, so
repeated catalog creation and withdrawal could start unbounded active bodies
despite a provider-wide limit. Detaching a body after timeout would also report
false quiescence and discard evidence of effects that may already have run.

## Decision

The provider admits at most 1,024 active-or-retained invocations. The active
settlement task owns one semaphore permit until the Tool body truly settles; a
live catalog then transfers it into the settled retained entry until commit or
catalog withdrawal. Withdrawing a catalog hides Pending state and signals
cancellation but does not recycle active admission.

Shutdown closes admission, releases settled entries, signals active bodies,
and waits on the exact settlement ledger. Deadline expiry reports unresolved
work without deleting it or fabricating outcomes. A task-owned RAII guard
cleans the ledger and permit if settlement machinery panics or is dropped.

## Alternatives considered

A grace period followed by detach or forced failure was rejected because safe
Rust cannot stop arbitrary trusted Tool code or prove it ceased effects.
Counting only queryable results was rejected because catalog authority and
resource ownership end at different times. A waiter queue was rejected because
admission is intentionally immediate and bounded.

## Consequences

The executor maps the two `ToolError` admission variants to stable
`tool.capacity` and `tool.shutting_down` turn-failure codes. With 1,024
withdrawn but non-settled bodies, the next start fails `ToolError::Capacity`;
exactly one true settlement restores one slot. A trusted body that never
settles permanently consumes one bounded permit. This is the unavoidable
consequence of honest effect ownership, and shutdown exposes the remaining
ledger rather than claiming false completion.
