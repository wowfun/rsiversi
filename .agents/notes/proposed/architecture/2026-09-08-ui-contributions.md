---
name: Shared conversation semantics and application contributions
---

## Problem

Web/TUI independently interpret Facts and expose closed actions/views. Web tool
completion loses intent. Full frames repeat work for unchanged content.

## Proposal

Share bounded native/wasm identities, source fields, Tool intent/result state,
and Output/Media references in pure Rust. Each application retains layout,
selection, and rendering. Plugins register owned surfaces/actions/renderers;
Rust validates generation-bound operations and document code owns DOM.

Linked renderers are trusted application code under the existing CSP. Data uses
text nodes, closed Markdown AST, and validated URLs. Settings schema describes
fields while the existing validator and revision CAS remain authoritative.

Files resolve authenticated Session bindings, including live drafts. Fingerprint
is a staleness check; WorkspaceTrust governs instructions, not manual browsing.
File Tools obtain a workspace-read scope from resolved Tool/Sandbox policy and
retain normal approval. They do not call the human API.

The terminal writer diffs complete desired frames against the last completely
written frame. Web preserves ACK pull and adds snapshot/patch payloads. Durable
cursors remain independent of display acknowledgements.

## Alternatives considered

A document plugin runtime duplicates ownership. One shared rendered view loses
platform semantics. Producer-side terminal deltas fail when watch skips frames.
Registry entries and Header fingerprints do not grant file authority.

## Acceptance criteria

An independent addon contributes and withdraws actions, renderers, and settings.
Tests cover source/backfill, stale operations, Files policy, media, children,
Unicode, short writes, bounded patches, visuals, and separately labeled live runs.

## Risks

DOM renderers share application privilege. Files API and Tools have distinct
entry authorities. Cache identities must preserve source anchors, and display
ACK must never advance Fact cursors. Every limit has one owning implementation.
