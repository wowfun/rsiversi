---
name: Independent stdout EOF and full duplex settlement
---

## Problem

A duplex child can close stdout while retaining stdin and stderr. Tying stdout
EOF to process exit creates a cycle when a protocol reader waits for EOF before
closing the transport and the child waits for stdin closure before exiting.

## Decision

The [Process contract](../../../../crates/rsi-process/core/README.md) distinguishes
stdout completion from full process settlement. The stdout drain publishes EOF
independently of child exit and stderr. A task-owned completion guard publishes
an error if the drain is cancelled before polling or unwinds. The reaper retains
process/capture admission until the child and both drains settle.

## Alternatives considered

Publishing stdout EOF only from the reaper creates the closure cycle. Killing
every child on stdout half-close would prevent legitimate duplex use. Releasing
admission at EOF or last-handle drop could over-admit while child cleanup is still
running. MCP instead retires its own epoch when its protocol reader closes.

## Consequences

Half-close can be observed promptly without implying child exit or released
admission. Dropping the final handle starts asynchronous cleanup; observing full
settlement or successfully reacquiring admission is the relevant completion
signal. Linux process tests cover stdout half-close, retained stderr/lifetime,
and eventual admission release after last-handle drop.

Both batch and duplex reapers now retain stdout/stderr joins independently.
A completed stdout join cannot be polled a second time after stderr consumes the
grace deadline. Deadline handling aborts and joins only outstanding tasks and
classifies the actual results, preserving concurrent clean completion and earlier
I/O failures. Paused-time tests cover clean joins at a zero deadline, early stdout
success with blocked stderr, and an early failure followed by a timeout.
