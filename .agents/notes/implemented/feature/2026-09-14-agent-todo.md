---
name: Session Todo state with a compact terminal view
---

## Problem

The terminal lacks persistent structured task progress. Inferring it from
arbitrary Tool output loses state during replay and cannot support reliable
Session switching or compaction.

## Decision

An Agent-only Todo plugin owns one typed domain and the exclusive `todo_write`
Tool. Each call replaces the complete list; an empty list clears it. Items have
only content and pending/in_progress/completed status, with multiple concurrent
in-progress items allowed. The domain persists across Turns and resets on fork.
It admits at most 64 items, 512 UTF-8 bytes per content line and 64 KiB encoded
state. Its pure settlement contribution atomically commits state with ToolResult.
The UI only reads and folds the projection; model context reads the same domain.
Plan mode permits this planning-only Tool.

The minimal whole-list shape follows pinned DSH source
`packages/todo/tool-todo/src/types.ts`; RSI uses its existing typed domain and
atomic commit ownership instead of importing DSH's Todo event-log authority.

## Alternatives considered

Per-item IDs add no value when the complete list is replaced. Restricting the
list to one active item misrepresents parallel work. Treating Todo as disposable
UI inference would make historical replay and current model context disagree.

## Consequences

Tests cover whole replacement, clear, bounds, multiple active items, across-Turn
state, reset-on-fork, Plan allowance and atomic settlement failure boundaries.
Terminal fixtures cover compact progress, expansion, empty omission and narrow
geometry without hiding higher-priority user questions or errors.


Todo progress is the model's declared work status, not proof of task completion.
The UI must preserve that distinction and avoid adding decorative progress text.

The current owning contract and implementation are in the [owning package](../../../../crates/rsi-agent/todo/README.md).
