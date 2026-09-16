---
name: Recorded file declarations and current-file delivery cards
---

## Problem

A final response containing a path does not provide a bound, inspectable file
delivery. The existing Files tools and human browser have different authority
owners and must not transfer their permissions implicitly.

## Decision

The ordinary Files Tool contribution adds present. It validates at most eight
regular files through the invocation's Sandbox scope and returns one versioned
ToolResult containing paths, short descriptions and observed lengths. Results
declare existing files; their bytes are not copied or preserved.

The Files UI contributes an inline renderer over recorded ToolResult Facts.
Actions identify the exact intent, result and file index. The server resolves
paths from these records, verifies complete Tool identity, then opens the current
file through the independently authenticated human SessionFiles API. History
never invokes a Tool again. Existing Field and Button elements express the card.

## Alternatives considered

A second deliverables Fact or SQL table would duplicate the ToolResult. Reading
an old ToolRuntime generation cannot serve history after restart. Accepting a
client-supplied path would discard the declaration binding. Copying file bytes
would create artifact retention and snapshot semantics outside this feature.

## Consequences

No partial successful declaration is returned. Missing files, directories,
symlinks and invalid paths are rejected. Stale or forged action coordinates
cannot select another file. Reopening history does not execute Tools. Current
file changes, disappearance and access refusal remain visible in both clients.

### Trade-offs

A declaration records what existed at Tool execution, not a lasting grant or
historical byte snapshot. A later user open can legitimately fail or show newer
contents. The UI must state this distinction at the point of use.
