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
plugin, with private index/object storage and source-owner authorization. Append one Turn marker to the Session when a comparison finds changed files.
The capture owner holds the comparison and its summary together. See the [owner contract](../../../../crates/rsi/workspace-review/README.md).

## Alternatives considered

A parallel finalizer has no ordering proof. A HEAD comparison loses the dirty
baseline. Retaining patches in Agent Facts conflates rebuildable review materials
with execution authority. Workspace copies would add an unnecessary second source
tree and contradict the requested in-place implementation workflow.

## Verification

Prove that effects await begin and that end follows finalizers and retained Tools,
including cancelled delayed writers. Exercise existing dirt, additions, deletion,
renames, concurrent unrelated changes, quotas, source isolation and restart. Hash
user Git metadata before and after and demonstrate the surviving Session marker and released
comparison. Verify terminal, browser and native Linux desktop paths.

## Consequences

A shared workspace has concurrent writers; interval overlap is temporal evidence.
Filesystem capture cannot prove external peers have no background work.
Batched blob import retains no authority over user paths: validated bytes enter
a private Git process without filters or refs. Capture waiters are bounded by
admitted intervals, and separate API read permits prevent UI polling from losing
a baseline. A single bounded cached patch per comparison avoids rerunning Git
on every page. Ambient global Git settings remain excluded from launch authority. Bounds
and cancellation may produce partial observations, which remain visible.

An interval retains its exact claim execution lease for source capture. Review
imports a bounded bundle of validated relative names, versioned bytes and file
modes into Local private scratch; it does not import remote `.git` metadata, refs
or executable configuration. The executable bit belongs to the Files open version,
not a second path-based stat on the Service. This also closes the native race
between reading bytes and reopening a different inode for its mode. DSH
`packages/deliverables/workspace-changes/src/git.ts` provides bounded subprocess
and private snapshot patterns, but its direct Node filesystem scratch operations
do not establish RSI remote-source authority.

DSH's workspace recorder appends `workspace/changes` with the turn number.
Its summary and file routes require the live recorder. A second durable summary
would retain counts after the files they describe have expired and would need
separate startup, capacity and purge handling. Keeping the marker with its
Session lets the standard Session lifecycle release it.
