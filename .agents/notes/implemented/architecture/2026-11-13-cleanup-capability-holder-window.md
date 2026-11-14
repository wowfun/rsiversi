---
name: Captured capabilities survive ordinary dependency cleanup
---

## Problem

Immediate revocation during ordinary provider retirement prevents dependent
cleanup from using the exact capabilities it captured during activation. The
[capability foundation](2026-08-24-cordis-capability-foundation.md) originally
chose immediate revocation; that clause no longer describes the implementation.

## Decision

Ordinary retirement withdraws discovery and structural authority first, while
existing holders can finish dependent and effect cleanup. The owning
[lifecycle contract](../../../../crates/rsi-meta/docs/subsystems/runtime-lifecycle.md#retirement-and-shutdown)
defines the exact fences and order. Runtime shutdown retains its stronger
global admission closure; the ordinary holder window cannot reopen it.

## Alternatives considered

Immediate revocation makes cleanup that needs a dependency fail by construction.
A privileged cleanup Context adds another authority model. Keeping the original
captured capability until its owning cleanup settles uses the existing lifetime
graph and admission counters instead.

## Consequences

An existing holder can enter retiring code until the final admission fence;
retirement is not immediate revocation. New lookup and mutation remain fenced.
Cleanup deadlines and callback draining still bound shutdown observation, and
retained handles keep their original entry reservations. Public lifecycle tests
cover dependent cleanup calls and the stronger shutdown boundary.
