# rsi-agent-todo

This Agent-only plugin owns the authoritative `rsi.todo` v1 domain. Its
exclusive `todo_write` Tool replaces the complete list, including an empty
list to clear it. Each item contains a nonempty single-line `content` and a
`pending`, `in_progress` or `completed` status. Several items may be in progress.
The list admits at most 64 items, 512 UTF-8 bytes per content line and 64 KiB
encoded state. State persists across Turns and resets to `[]` on fork.
List admission trusts each immutable typed item and counts its exact JSON length
without allocating a serialized copy. The schema describes the UTF-8 byte limit;
its character-count keyword is only an additional coarse bound.

The Tool only validates and returns its proposed list. A pure ToolSettlement
contribution verifies the exact intent and result, then proposes the typed
replacement. Kernel publishes that state and ToolResult atomically. This plugin
has no Kernel, Session or Store mutation capability. Its context contribution
and read-only projection consume the same typed domain. Empty state contributes
no model input or terminal task display.
