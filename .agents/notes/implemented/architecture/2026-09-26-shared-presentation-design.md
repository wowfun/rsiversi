---
name: Shared presentation principles with platform-owned expressions
---

## Problem

Terminal information hierarchy and Web styles have separate implicit owners.
Web layers override each other's colors and geometry. A shared design contract
must not move rendering or input policy into the renderer-neutral UI protocol.

## Decision

The product keeps common information and feedback principles in the RSI product design
reference. Web owns GUI styles and device layout preferences; the terminal owns
cell geometry, input and focus. Exact values remain in code. Profile-owned
appearance is distinct from document-local panel geometry.

This narrowly supersedes the shared information-hierarchy ownership in the
[terminal reference decision](../process/2026-09-14-terminal-design-reference.md).
Its platform contract and independent-decision rules remain applicable.

GUI and TUI Stop use the Session attachment and Turn from the acknowledged
presentation. They preserve queued inputs and drafts. Earlier cancellation also
removed pending inputs; separating pending-only withdrawal prevents one visible
Stop action from affecting work the user did not target. Strong request
cancellation remains available to its existing Line, ACP and message drivers.

## Alternatives considered

A root DESIGN.md would hide product ownership. Copying tokens into Markdown and
code would introduce drift. Importing the complete reference theme would add
unneeded branded assets and unrelated vendor changes.

## Consequences

Profile-owned appearance applies live across clients. Layout stays in a separate
bounded per-device store so geometry eviction cannot remove drafts or credentials.
The existing DSH source remains pinned with per-file revision and digest provenance;
product adapters own theme and role geometry, avoiding an unrelated vendor rebase.
System fonts avoid importing reference branding or font assets.

Computed styles, contrast, focus and resize probes in Chromium and Firefox,
actual Linux WebKitGTK system appearance and PTY captures validate platform
behavior. Native Windows and macOS behavior is not claimed. Unknown mutations
remain recoverable rather than disappearing during a presentation change.

The document aligns sidebar, directory chooser and composer geometry with DSH
`477b4f4`, retaining RSI branding, English copy and system fonts. Existing
`c291e79` vendor bytes remain unchanged. Layout version 2 distinguishes expanded,
rail and hidden navigation; responsive collapse and the mobile drawer do not
rewrite the saved preference. One composer node and parent survive empty and
active states. The [Turn index](../feature/2026-09-26-turn-presentation-index.md)
owns semantic folding; the [composer decision](../feature/2026-09-26-composer-delivery-preferences.md)
owns delivery defaults and their Settings migration.

Copying code goes through one 64-KiB write-only clipboard capability. Desktop
uses a native implementation because its custom origin cannot promise browser
clipboard support; a failed write never produces a success label. Screenshot
comparisons are review evidence, not automatically updated pixel baselines.
