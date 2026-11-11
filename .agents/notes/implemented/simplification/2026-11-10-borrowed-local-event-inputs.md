---
name: Borrowed inputs for direct Local events
---

## Problem

Parallel and Serial dispatch cloned an event's value for each selected listener.
This imposed payload copying even though the dispatch already owned that value
until completion. The generic Clone bound also excluded uniquely owned inputs
from modes that never needed to copy them. Cordis's event implementation passes
the same arguments to its selected callbacks; a Rust borrow expresses that
sharing without introducing a dynamic object runtime.

## Decision

Parallel and Serial handlers borrow the dispatch-owned input across their async
work, matching the existing Emit and Bail borrowing model. Waterfall continues
to move its value. LocalEvent no longer requires Clone. The captured listener
snapshot also owns handlers throughout dispatch, so callback invocation borrows
the handler instead of taking another Arc reference.

The current contract is in [Local events](../../../../crates/rsi-meta/docs/subsystems/events-and-contributions.md).

## Alternatives considered

Requiring every event to carry an Arc would hide accidental deep copies but
impose shared allocation on small or uniquely owned values. Moving the original
value into the last callback saves only one copy and complicates once admission.
Keeping an owned compatibility interface preserves the unnecessary copying and
adds another dispatch path in a project without pre-release compatibility
guarantees.

## Consequences

Async handler implementations accept a reference. A handler that intentionally
retains data after its callback returns chooses its own clone or shared owner.
Dispatch remains caller-owned, with unchanged ordering, once admission, bounded
parallelism, cancellation and listener destructor containment. This removes
specific copies and reference-count operations; it does not establish an RSS or
end-to-end latency improvement, or replace the companion JS runtime.
