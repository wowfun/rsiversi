---
name: Keep native generation leases separate from Web bundle identity
---

## Problem

The build-info package emits an unused family digest while its actual native
consumers retain generation leases. Its documentation consequently attributes
native compatibility and Worker identity policy to a library that owns neither.

## Decision

Build-info verifies frozen manifest inputs and embeds the managed-publication
flag required by GenerationLease. It exposes no family digest. Application
distribution owns receipts and the Web Worker's internal bundle identity; the
standard core has no dependency on the unused native digest projection.

## Alternatives considered

Routing the Worker through the unused native accessor would introduce coupling
without a consumer need. Removing build-time manifest verification along with
the accessor would instead weaken the managed flag and source-input contract.

## Consequences

Generation locking, collection exclusion and managed startup checks remain.
The unused pre-release accessor is removed without a compatibility alias.
Publication and Worker checks continue to consume their existing identities.
