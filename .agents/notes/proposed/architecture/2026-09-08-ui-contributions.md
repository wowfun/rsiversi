---
name: Shared conversation semantics and application contributions
---

## Problem

Web/TUI expose closed actions/views, and full frames repeat work for unchanged
content. Earlier independent Fact projections lost Tool intent and source identity.

## Proposal

Share bounded native/wasm identities, source fields, Tool intent/result state,
and Output/Media references in pure Rust. Each application retains layout,
selection, and rendering. Plugins register owned surfaces/actions/renderers;
Rust validates generation-bound operations and document code owns DOM.

The shared conversation foundation is implemented: exact closed source windows,
bounded source membership, complete Tool identity and phase, and Media provenance.
Renderers retain their own bounded text/layout state and no observation leases.
Detail cancellation is presentation-local and preserves admitted mutations.

Settings discovery is implemented over the existing registered namespace owner.
Its schema and application timing are descriptive; they do not replace safe-Rust
validation or registration/revision CAS. Bounded lexical pages avoid a full raw
document or registry export. Describing a namespace acquires no registration
lease, and an editor verifies that metadata and value share a registration.
Sensitive markers govern presentation of non-secret fields; RSI continues to
keep credential material outside Settings. These foundations do not complete the
surface/action/renderer contribution acceptance criteria below.

The UI registration owner uses ordinary Meta factories and registration effects.
It captures declaration-order snapshots and binds actions to a fresh application
nonce, exact contribution registration and actual target Context. Views contain
closed text/form/button primitives; domain-specific target factories declare the
Local dependencies whose replacement must retire their target. Native and Worker
public-seam tests exercise an independent addon. The Web adapter and first-party Session/Tool inspection contributions are
implemented with exact-source paging and detail cancellation. The TUI adapter
and the remaining workbench acceptance criteria still need implementation.

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
