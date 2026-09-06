---
name: Retire permanently failed waits without replaying their Turns
---

## Problem

A parked activation requires a durable resume before execution can continue.
Retrying every failure indefinitely also retains local ownership after the
Store has rejected the transition permanently or its activation has disappeared.
Releasing that ownership alone would let the executor reclaim the same parked
Turn and attempt another model run. Fact watermarks alone cannot report this
failed control transition during shutdown.

## Decision

The retained wait preserves typed Store failures until it chooses retry or
permanent failure. I/O, cursor contention and admission waits retain the existing
owned retry path. A deterministic authority, lifecycle or validation error
pauses the affected Session with a bounded permanent diagnostic before the wait
releases its local resources. The latch belongs to the exact mutation owner;
an obsolete lease cannot pause a replacement claim. Later watermark publication
preserves that latch, and shutdown reports it even when the Fact tail is durable.
Each retained wait exclusively owns its claim's wait slot before attempting a
park, through actual cleanup. This makes read-back after a lost acknowledgement
belong to that wait; a rejected overlapping request cannot resume another
request's parked activation.

This partially supersedes the unconditional resume-retention rationale in the
[CLI coding workflow note](../feature/2026-09-05-cli-coding-workflow.md).
The [Kernel contract](../../../../crates/rsi-agent/kernel/README.md) owns current
retry, caller deadline, ownership and failure behavior.

## Alternatives considered

Stopping all retries at caller cancellation would abandon a recoverable durable
park and violate admitted-work ownership. Dropping a permanently failed lease
without pausing its Session would permit another executor claim against that
unrepaired activation. Retrying deterministic failures forever cannot restore
the missing authority and prevents the owned task drain from finishing.

## Consequences

A permanent failure does not manufacture a successful resume or terminal Fact.
The durable activation remains unfinished for normal startup repair; genuinely
corrupt Store state must be repaired before recovery can proceed. A successful
cleanup drain can still report the retained Session failure. Transient storage
unavailability retains local resources until actual recovery, independently of
the public waiter's deadline. Regressions inject missing activation, corrupt
read and invalid commit failures, reject executor redrive, and check shutdown
reporting and subsequent interrupted-Turn recovery.
