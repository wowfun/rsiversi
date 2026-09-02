---
name: Bounded shutdown-aware submission admission
comment: Admission queues must not outlive the Kernel that owns them
---

## Problem

Submission admission bounded active work to 256 slots and serialized Store
checks by Session, but both wait paths were unbounded. The semaphore was never
closed, so a caller queued behind saturated or same-Session work could remain
pending after Kernel shutdown had already stopped accepting requests.

## Decision

One cancellation token closes the complete submission-admission lifetime.
Shutdown cancels that token and closes the slot semaphore before its final
flush. Waiting for either a process slot or a same-Session guard races that
closure and the existing one-minute durability deadline. Shutdown reports
`TurnError::ShuttingDown`; deadline expiry reports bounded capacity pressure.

## Alternatives considered

Fail-fast admission was rejected because short Store overlap should
backpressure rather than spuriously reject independent submissions. Cancelling
work after it has entered Store I/O was rejected because the Kernel cannot
claim that an arbitrary Store future ceased or had no durable effect. The
bounded wait therefore governs only acquisition of Kernel-owned admission.

## Consequences

Accepted Store work remains owned until it settles, but no admission waiter is
stranded behind it. A caller that cannot enter the bounded active set within
the durability deadline receives `TurnError::Capacity` without creating a
session reservation or speculative Fact.
