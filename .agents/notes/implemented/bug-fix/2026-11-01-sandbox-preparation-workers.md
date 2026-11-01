---
name: Retain bounded Sandbox preparation through real filesystem completion
---

## Problem

Async confinement methods performed native filesystem checks inline. A blocked
filesystem could occupy a Tokio worker and prevent unrelated cancellation or
shutdown progress. Moving work to an unbounded blocking queue would only move the
resource problem.

## Decision

The [local provider](../../../../crates/rsi-sandbox/local/README.md) admits four
blocking preparations per generation. A worker owns its permit, generation and
staged executable until it actually returns; cancellation releases only its waiter.
Effective cgroup reads use the same admission boundary.

## Alternatives considered

Synchronous checks are simple but block async execution. An async timeout cannot
stop an operating-system filesystem call. Detached unlimited blocking work loses
both backpressure and staged-backend lifetime protection.

## Consequences

Busy preparation fails immediately with a typed capacity error. Hung filesystem
calls may occupy the finite slots until the OS returns; no hard cancellation
promise is made. Escaped typed service handles retain their existing Local
lifetime contract after publication withdrawal.
