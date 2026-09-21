---
name: Awaited execution boundaries and runtime workspace comparisons
---

## Problem

Concurrent finalizers cannot capture a stable baseline before the first effect or
prove that a terminal Turn has stopped all retained Tools. A diff against HEAD
would also attribute preexisting dirty changes to the current execution.

## Decision

The optional Executor observer orders: admit, await begin, drive, join bounded
controlled-work settlement, await end. Git and evidence reside in an ordinary product
plugin, with private index/object storage and source-owner authorization. Persist
bounded summaries separately from Agent Facts and retain diff data only for the
owning runtime epoch. See the [owner contract](../../../../crates/rsi/workspace-review/README.md).

## Alternatives considered

A parallel finalizer has no ordering proof. A HEAD comparison loses the dirty
baseline. Retaining patches in Agent Facts conflates rebuildable review materials
with execution authority. Workspace copies would add an unnecessary second source
tree and contradict the requested in-place implementation workflow.

## Verification

Prove that effects await begin and that end follows finalizers and retained Tools,
including cancelled delayed writers. Exercise existing dirt, additions, deletion,
renames, concurrent unrelated changes, quotas, source isolation and restart. Hash
user Git metadata before and after and demonstrate readable old summaries with
explicitly expired diffs. Verify terminal, browser and native Linux desktop paths.

## Consequences

A shared workspace has concurrent writers; interval overlap is temporal evidence.
Filesystem capture cannot prove external peers have no background work.
Batched blob import retains no authority over user paths: validated bytes enter
a private Git process without filters or refs. Capture waiters are bounded by
admitted intervals, and separate API read permits prevent UI polling from losing
a baseline. A single bounded cached patch per comparison avoids rerunning Git
on every page. Ambient global Git settings remain excluded from launch authority. Bounds
and cancellation may produce partial observations, which remain visible.
