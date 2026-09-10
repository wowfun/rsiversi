---
name: Generation-pinned Agent contributions and durable domain state
---

## Problem

Independent business plugins need durable state, commands and presentation without
making the Kernel or UI own each plugin's business logic. External effects cannot
be reconstructed by replaying arbitrary plugin callbacks after a restart.

## Decision

The [typed domain substrate](2026-09-08-typed-domain-commits.md) supplies bounded
complete values and canonical receipts. Ordinary generation-owned registrars
connect those values to context, monotone Tool policy, commands and projections.
The [correlated terminal boundary](2026-09-08-terminal-control-boundaries.md)
selects exact historical fork state. Commands propose typed state replacements;
only the Kernel admits their atomic durable commit. A draft retains the same
command catalog and initial values that its first acceptance publishes.

The [Agent product](../../../../crates/rsi-agent/README.md) owns these contracts.
Plan policy and repeated-Tool reminders consume those interfaces as ordinary
plugins. Main execution and background context maintenance use the same selected
builder and immutable generation. UI adapters consume descriptors, receipts and
complete views instead of reconstructing business state from display text.

## Alternatives considered

Arbitrary JSON writers bypass domain validation; separate Storage creates dual
authority. Sampling time inside a builder loses provenance. Replaying command
callbacks could repeat external work and cannot establish the original receipt.
Cache-envelope compatibility cannot replace an authoritative schema cutover.

## Consequences

Resident work retains its old generation; cold recovery resolves the current
healthy generation and validates its support for existing state. A completed,
evicted Session is cold even while a UI still holds its handle. Missing or
incompatible contributions do not authorize effect replay. Projection producer
failures remain isolated from core history. The independent workbench addon
exercises real draft publication, persisted context, policy denial, held resident
work during reload, fork boundaries and cold restart through public interfaces.
