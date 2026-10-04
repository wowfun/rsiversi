---
name: Transient Store refusals do not permanently fence Sessions
---

## Problem

Cold validation refusal includes shared queue pressure, cancellation of abandoned
reads and SQLite read-lock contention. A one-minute interval cannot distinguish
those causes from a permanent fault in the requesting Session. Human-wait cleanup
also carried a capacity interval through unrelated I/O and conflict retries.

## Decision

Known pre-write Store admission refusals remain retryable without a permanent
Session latch or an I/O-failure charge. Caller durability waits retain their
existing deadline; owned work and durable pending Facts remain until completion
or authoritative recovery. Human-wait cleanup distinguishes shared Store refusal
from local Kernel capacity. Only uninterrupted local-capacity refusals use its
one-minute fence, and every other retry cause resets that interval.
This replaces the transient-refusal fencing policy in the
[bounded-admission decision](../architecture/2026-10-02-bounded-validation-and-context-admission.md).
The [security contract](../../../../crates/rsi-agent/docs/security.md) owns retry
classification; native attempts are never abandoned to meet a waiter deadline.

## Alternatives considered

A new Store error for SQLite contention would improve diagnostics but would not
make shared validation queue pressure a Session fault. A global progress counter
would reset unrelated Session timers while still attributing a stalled shared
lane to an innocent Session. Neither justifies a permanent latch. Returning an
apparently successful human resume before its durable control would break park
ownership and allow unsafe execution.

## Consequences

Persistent shared Store pressure can keep owned cleanup pending beyond a caller's
deadline. Shutdown reports its existing bounded drain failure instead of claiming
the durable park was resumed. Restart recovery remains authoritative. Paused-time
tests retain pending Fact ownership past one minute, distinguish mixed retry
causes, and prove human-wait recovery after shared refusal clears.
