---
name: Watched build cancellation belongs to the Host build owner
---

## Problem

The devtools distribution runner retained a cancellation option that its
synchronous build path never supplied. This suggested a second cancellation
owner without implementing a usable cancellation path.

## Decision

The synchronous distribution runner waits for its build lock and child exit,
drains both output pipes and reports the retained diagnostics. Watched builds
retain cancellation and process-tree settlement in the ordinary Host build
plugin, which owns those processes. The current separation belongs to the
[devtools contract](../../../../apps/devtools/README.md).

## Alternatives considered

Keeping an unused option preserves no behavior. Wiring distribution into the
watched-build cancellation path would add a second lifetime contract to a
command whose existing callers wait for completion.

## Consequences

Removing the unused path does not claim that synchronous distribution supports
cooperative cancellation. New cancellation requirements must name the actual
process owner and establish drain/settlement behavior before exposing an option.
