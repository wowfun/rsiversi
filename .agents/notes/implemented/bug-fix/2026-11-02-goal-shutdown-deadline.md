---
name: Bound Goal driver shutdown without losing mutation identity
---

## Problem

A Goal driver can await a retained Kernel command before reaching its existing
cleanup timeout. Dropping that waiter does not stop the Kernel commit. Reading
an older snapshot could then falsely report that no reservation needs cleanup.

## Decision

The [Goal controller](../../../../crates/rsi/goal/README.md) gives its existing
driver and final settlement one deadline from stop. It revokes scheduling but
continues polling the same driver until completion or timeout. Dispatched
mutations retain their original identity and digest; only a definite result or
matching canonical receipt resolves them. Cleanup reports remaining uncertainty.

## Alternatives considered

Timing only final settlement leaves earlier reads and commits unbounded.
Dropping the driver immediately and reading current state can miss a later
retained commit. Retrying under a new identity can reserve another round. None
of these alternatives preserves both bounded Host cleanup and truthful outcomes.

## Consequences

A deadline can return while Kernel work remains unresolved. The diagnostic
retains the request and message identity and never claims a clean shutdown.
Final live-state publication cannot wait indefinitely on the control gate.
Real Store and provider behavior remain distinct from deterministic controller
barrier tests.
