---
name: Independently owned resource views and DSH workbench alignment
---

## Problem

One global Details operation cancels the previous view, and terminal rendering is
tied to current selection. Multiple visual tabs alone cannot provide independent
authority or preserve terminal state while moving between panes.

## Decision

Rust owns a bounded registry of independent views, each retaining its own source
authority, version and cancellation. Dock IDs only express layout. Moves retain
the view; duplicate and undo-after-close acquire fresh authority. Settings has
an independent modal owner. Stable tab hosts keep terminal followers mounted.
Closing a terminal view detaches it; a terminal list permits reattachment, while
an explicit terminate action stops the shell.

The workbench implements the DSH product's two horizontal panes, tab movement/merge,
document floats, resize, fullscreen and undo/redo. Only dockkit and its minimal
dependency closure are vendored, with per-file revision and hash provenance.
Existing vendored primitives keep their revisions. RSI owns the composition adapters.

Independent bounded IndexedDB Session layout records contain coordinates, never
live handles. Narrow-screen overlays and active-pane selection preserve the
saved desktop layout. The
[directory-picker authority](../../implemented/architecture/2026-09-26-directory-picker-authority.md)
remains independent of presentation.

## Alternatives considered

Cloning the existing detail component shares cancellation and authority. Native
floating windows add an unrelated platform lifecycle. Importing the upstream
application state would replace RSI's owners rather than reuse presentation.

## Consequences

Closing one view leaves siblings intact; stale handles perform no I/O. Moving a
terminal does not reattach or lose output. Restore reacquires exact content
authority and never creates or takes over a terminal implicitly. Fixed Linux
fixtures verify geometry within 2 CSS px and agreed computed tokens. Matching
Chromium DSH fixtures require at most 1 percent differing pixels at channel
threshold 16; new SSH/narrow layouts use reviewed RSI goldens. WebKitGTK gets
its own geometry and interaction verification, not cross-engine pixel thresholds.

Golden changes require explicit review. RSI's limits of 16 tabs and 4 floats are
product admission policy, not upstream limits. Narrow-screen behavior must remain
usable without destroying the user's wider layout or overwriting concurrent edits.
