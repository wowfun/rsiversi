---
name: Window-owned Turn presentation
---

## Problem

Per-block folding cannot distinguish a completed answer from intermediate text
that subsequently requests Tools. Rewriting message content to mark presentation
roles would invalidate renderer caches and disturb a reader's position.

## Decision

The GUI keeps a separate, window-bounded Turn index with its own revision and patches.
It uses typed Facts to identify answers, candidates, process and partial windows.
The document receives explicit running/foldable policy fields so changes to
English status labels cannot change visibility.
It preserves human steering and boundary rows in chronological position. Detail mode
belongs to device-local layout; Verbose absorbs the separate Trajectory switch.
This narrowly supersedes the independent Chat/Trajectory presentation choice.

## Alternatives considered

Inferring semantics from browser block keys couples the document to internal
identity encoding. Adding markers to durable Agent Facts makes a presentation
preference part of execution history. Both cross the wrong ownership boundary.

## Consequences

Model text followed by Tools, compaction, steering, unsuccessful terminals and
partial history remain truthful. Index patches preserve unchanged block objects
and DOM nodes. Folding and streaming preserve the reading anchor.


A truncated window cannot always establish an answer. It must expose partial
history rather than collapse content based on a guess.

DOM verification observes unchanged child-list identity, candidate reclassification
and reading-anchor retention within the browser's subpixel scroll rounding.
The [shared presentation decision](../architecture/2026-09-26-shared-presentation-design.md)
continues to own platform boundaries and display preferences.
