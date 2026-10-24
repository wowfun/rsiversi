---
name: Replayable stateless API routes
---

## Problem

Uniform restart requirements prevent route-only adapters from being disabled or
regrouped even though their domain state belongs to stable upstream providers.
History previously released its API handler before the worker actually settled.

## Decision

Route-only adapters are Replayable after full invocation settlement is established.
History mutations retain worker completion ownership through retirement and deadline.
Local/event isolation names are preflighted before factory preparation; activation
rollback establishes graph validity without a separate isolatable-type allowlist.

## Alternatives considered

A timeout that releases names while old mutations run permits overlapping owners.
Blanket Agent publication bans would prohibit intentional sharing. Merely registering
a marker does not prove a dependency graph will activate.

## Consequences

Draining can delay Profile apply. Reopening a surface recreates observations after
bounded automatic retries stop. Linked code still requires a new binary/catalog. Session
generation pins remain. Isolation separates runtime slots, not factory Arc state.
