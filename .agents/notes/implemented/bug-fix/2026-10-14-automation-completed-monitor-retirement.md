---
name: Retire completed Automation monitoring before final projection capture
---

## Problem

A protected exploration can finish its model work and disarm its Goal while the
round-deadline monitor still owns a durable Fact observation. Retaining completed
monitoring across the final projection capture stalls that read until the capture
deadline. The deterministic integration scenario reproduces the stall after three
provider requests and an observed disarmed Goal.

## Decision

The live Goal observation and round-deadline future have one lexical lifetime
ending when the Goal disarms. Both retire before reading the final projection.
The [Automation contract](../../../../crates/rsi/automation/README.md) owns this
ordering. Cancellation, verified-report validation and preview cleanup retain
their existing owners.

## Alternatives considered

Increasing capture deadlines retains the obstruction. Polling or reconstructing
Goal state outside its existing projection would introduce another read path.

## Consequences

The protected exploration integration test completes in under a second after
releasing monitoring; retaining it reproduces the thirty-second timeout. The
failure-after-report scenario continues to require failed source-Turn settlement
before rejecting an unverified report.
