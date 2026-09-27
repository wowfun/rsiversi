---
name: Release suspended ACP observation before cancellation reads
---

## Problem

An ACP prompt can select cancellation while its observation stream is suspended
inside a Store page read. The stream owns that in-flight future even after the
`next()` waiter is dropped. A completed but unconsumed read can retain the entire
Store payload budget. Waiting for message cancellation while keeping that stream
alive blocks the Header read cancellation needs, until ACP close times out.

## Decision

ACP drops the observation stream before awaiting message cancellation. It retains
the last consumed Fact and control cursors and resumes observation from those
coordinates after cancellation returns. Cancellation continues to target the
original message and requires durable termination and settled controlled work.
Dispatched Store work still owns its admission until it finishes.

## Alternatives considered

Increasing the payload budget or timeout hides the ownership cycle without
removing it. Releasing permits before dispatched reads finish breaks Store memory
bounds. Resubmitting the message or replaying from the original cursor changes
user-visible delivery or duplicates updates.

## Consequences

Cancellation does not depend on polling a suspended observer to release its
Store admission. Reopening uses the existing reconnectable dual-cursor contract.
The native ACP lifecycle regression also checks one accepted Turn and a durable
cancelled terminal after handler loss and close.
