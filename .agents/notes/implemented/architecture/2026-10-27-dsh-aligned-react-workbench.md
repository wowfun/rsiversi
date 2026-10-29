---
name: DSH-aligned React workbench with document-owned lifetimes
---

## Problem

The first-party Web UI mixes React components with a large imperative Session
renderer, while stylesheet overrides and a chronological README obscure their
owners. The running npm DSH reference and vendored sources also represent
different revisions, making visual acceptance ambiguous.

## Decision

The workbench uses official DSH commit `22ddee3792402c3e70ec5f86a63c3e3e895a2f60` for the
source-built reference, vendored closure, provenance and automated comparison.
It retains RSIversi branding and existing product authorities, excluding upstream
brand art, fonts and inline theme injection. Renderer classes and theme variables
remain an explicit global styling contract alongside first-party CSS Modules.

First-party rendering uses React for component ownership and maintainability;
visual alignment alone does not require that rewrite. A document runtime owns
transport, submission and awaited cleanup. A frame/connection-keyed commit barrier
requires successful React commit and renderer settlement before ACK. Stable
Session, editor and message identities survive frames. Independent renderers and
terminals retain their own DOM. Runtime changes force full development reloads.

The external-agent sidebar launcher is intentionally absent at the user's request.
Existing external panes and attention navigation remain supported; this does not
advertise proactive external-agent discovery or start from the Web sidebar.
The unused launcher component is removed rather than retained as dormant UI.

Rust's bounded, closed Markdown events include tables and code language labels;
JavaScript only renders that event set and bounded syntax tokens. Document consumers
are rebuilt together. Presentation storage uses schema 3 and workbench layouts v4, resetting
old workbench geometry while retaining Dock documents, preferences, orders and
the separate draft journal. Reads stay read-only; explicit layout reset repairs
invalid selected content. Old open database owners must reload after upgrade.

This partially supersedes the deliberate Session DOM island in
[plugin-composed GUI](2026-09-11-plugin-composed-gui.md)
and the vendor/theme scope in
[shared presentation](2026-09-26-shared-presentation-design.md).
Their plugin authorities, platform boundaries and unrelated decisions remain.
The [Turn presentation](../feature/2026-09-26-turn-presentation-index.md)
detail modes remain; DSH's Chat/Trajectory switch is not imported.
The later [conversation-start decision](../feature/2026-10-28-model-independent-conversation-start.md)
adds product-owned Conversation/Activity tabs over those existing detail modes.

## Alternatives considered

Restyling the imperative island would satisfy the visual goal with less immediate
lifecycle work, but retain split ownership and source-splicing tests. Importing
DSH packages would add Cordis runtime dependencies. Mixing npm screenshots with
newer source would not define a reproducible acceptance target. Migrating old
geometry adds compatibility machinery for disposable presentation preferences.

## Consequences

React commit failures must not acknowledge unapplied frames or enable stale actions.
Renderer CSS cannot become private component CSS without replacing its consumers.
Storage version changes retire open connections; preference reset must never touch
saved input. Timing evidence is machine-specific and complements bounded-work
assertions. Source-built upstream runtime evidence may need separate build tools;
an npm release is not a substitute for the pinned commit.

The [document runtime contract](../../../../apps/web/docs/runtime.md) owns commit,
settlement and HMR; [persistence](../../../../apps/web/docs/persistence.md) owns
layout reset. Presentation and renderer contracts live in their own Web documents.
Fixtures use explicit document factories and the production React tree instead of
extracting production source strings. Only the frozen historical performance adapter
retains its original source adaptation.

Live provider acceptance exposed ordinary short tables falling back to plain text
under a proportional budget with too little fixed allowance. The
[GUI Markdown owner](../../../../crates/rsi/gui/README.md) now accounts for structural
overhead within explicit aggregate bounds. Arbitrarily complex or oversized Markdown
still falls back to exact source; no JavaScript parser or network grammar is added.

Targeted checks cover Chromium/Firefox commit failures and disposal, layout/reset,
Settings, retained inputs, current product setup/navigation and real provider turns.
Native Windows WebView2 and Linux WebKitGTK were exercised separately; neither
establishes native macOS behavior. Actual pinned DSH and RSI both completed live
Markdown, read-only file, Stop and recovery scenarios. Reviewed Linux Settings
goldens cover seven widths and both themes. The reference component gate retains
its pixel/geometry tolerances. Frozen-baseline comparisons cover four scenes (16, 64, 128 blocks and 128 blocks
near the text budget) with 50 samples per engine and variant. The local acceptance
comparison met its p95 delta threshold of max(20% of baseline, 16.7 ms); the
checked-in performance runner remains report-only and is not a CI timing gate.
This is targeted validation, not an exhaustive platform matrix. The broad Unix file
fixture cannot create its raw-byte filename on Windows and remains a Unix CI target.
