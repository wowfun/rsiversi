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
Shutdown cancels the producer token before its final flush. Retained settlement
proofs can still enter after producer closure; the operation semaphore remains
open for them. Waiting for all Session guards and then one process slot shares
one one-minute deadline. Shutdown reports
`TurnError::ShuttingDown`; deadline expiry reports bounded capacity pressure.

## Alternatives considered

Immediate rejection while pending capacity remains was rejected because short
Store overlap should backpressure. A separate 256-entry pending bound rejects
excess demand before keyed allocation, so a burst cannot retain unbounded waits.
Acquiring one process permit per Session was rejected: sorted Session locks do
not prevent multi-Session operations from each holding half their semaphore
reservation. Each operation instead locks its at-most-two sorted, deduplicated
Sessions before taking one process permit. Taking the process permit before the
Session guards would let hot-Session waiters occupy all independent capacity. Cancelling
work after it has entered Store I/O was rejected because the Kernel cannot
claim that an arbitrary Store future ceased or had no durable effect. The
bounded wait therefore governs only acquisition of Kernel-owned admission.

## Consequences

Accepted Store work remains owned until it settles, but no admission waiter is
stranded behind it. A caller that cannot enter the bounded active set within
the durability deadline receives `TurnError::Capacity` without creating a
session reservation or speculative Fact.

Keyed ownership releases each guard before its last key owner; that owner's Drop
removes only its exact weak allocation. Pointer identity prevents a retired
entry from removing a replacement during concurrent registration. This avoids
whole-registry sweeps without a separate cleanup task or long-lived cache.

Pending and active bounds are private and both 256. They bound different phases;
they do not enlarge downstream API or provider capacity. Owned mutations retain
their admitted lease through durable settlement when their initiating caller is
cancelled.
