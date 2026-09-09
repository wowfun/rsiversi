---
name: Shared conversation semantics and application contributions
---

## Problem

Closed application actions prevent independent business extensions. Sharing
rendered frames would erase platform behavior, while separate Fact projections
can lose Tool identity, source anchors and mutation ownership.

## Decision

The [conversation library](../../../../crates/rsi/conversation/README.md) owns pure
bounded semantic identities and exact source windows. The ordinary
[UI registry](../../../../crates/rsi/ui/README.md) owns surfaces, actions and
renderers with exact contributing and target Context lifetimes. TUI and Web keep
their own layout, editing and rendering. Headless exposes command receipts and
canonical Facts. All consume the same Session business authority.

Rust admits generation-bound actions and validates closed presentation data.
Linked renderers are trusted application code; the Web document uses text nodes,
closed Markdown and validated URLs under the existing CSP. Detail cancellation
stops presentation work while preserving already-admitted mutations. Settings
schema and application timing describe the namespace owner's validator and CAS;
they never become a second validation authority.

The terminal diffs complete desired frames against its last completed write.
Web retains ACK pull with snapshots and patches, exact base validation and
explicit resynchronization. Display acknowledgements do not advance durable
observation cursors. The owning implementations define each bound once.

## Alternatives considered

A document plugin runtime duplicates ownership. One shared rendered view loses
platform semantics. Producer-side terminal deltas fail when watch skips frames.
Registry entries and Header fingerprints cannot grant file authority; manual
Files browsing and model Tools retain their distinct entry policies.

## Consequences

Applications can add ordinary action and renderer factories without business
branches in their controllers. The complete workbench includes source, output,
Settings, Files, Media and child-session inspection. Actual Chromium/Firefox and
Linux PTY fixtures exercise those adapters separately from deterministic registry
lifecycle tests and opt-in live providers. The independent addon uses the same
public composition declaration across Session tests and real application clients.
This does not introduce dynamically downloaded frontend code or establish native
Windows/macOS terminal rendering.
