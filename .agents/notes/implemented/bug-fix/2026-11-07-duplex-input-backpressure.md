---
name: Duplex input waits for pipe progress within process ownership
---

## Problem

A fixed 500 ms pipe-write deadline treats a busy protocol subprocess as failed
capacity even when it can resume normally. Extension treats an interrupted frame
as a disconnected peer and terminates it; retrying the frame cannot establish
replay safety. Windows also acknowledges a command before releasing its input
slot, so a woken sequential writer can observe false contention.

## Decision

The [Process contract](../../../../crates/rsi-process/core/README.md) owns input
admission and cancellation. Unix and Windows share one input owner, retaining
the bounded command through its write and releasing admission before
acknowledgement. Closing flushes accepted bytes before dropping the pipe.
An abandoned caller leaves the admitted command with that owner.
Process termination and direct-child exit end pipe work; protocol owners retain
their operation deadlines. The DSH LSP connection's stream write callback and
SSH protocol's drain wait provide the relevant model: pipe backpressure is
progress to await, not an independent process-health deadline.

## Alternatives considered

Raising the deadline only moves the failure threshold. Retrying in Extension
cannot distinguish a rejected frame from an accepted prefix. Unbounded queued
writes would hide backpressure and retain additional payloads. Keeping separate
platform workers duplicates this protocol ownership and allows their admission
ordering to diverge. Tokio's Windows pipe writer buffers an accepted write in a
blocking I/O task. Flushing every chunk adds a blocking task per write without
establishing consumption by the child. Flushing only on close settles the final
accepted bytes; a later native failure still ends the pipe without replay.

## Consequences

A live child that never reads retains one input slot until its protocol owner
terminates it. This makes no new resource-growth path and needs no timer or
retry task. A cancelled or failed write still does not establish replay safety.
PTY interaction deadlines are a different user-input contract and remain with
their existing owner. Reproducing this failure mechanism does not establish the
cause of a historical disconnect whose underlying I/O error was not captured.
