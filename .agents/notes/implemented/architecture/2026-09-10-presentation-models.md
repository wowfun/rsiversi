---
name: Owned presentation models and renderer generations
---

## Problem

Closed UI views cannot express arbitrary named renderer models. Individual HTTP
response pins cannot preserve a complete module graph for later lazy imports.
Replacing terminal code must not replace terminal modes or Session ownership.

## Decision

Keep bounded neutral models separate from executable renderer admission.
Presentation leases materialize asynchronously and drawing consumes captured
snapshots. Invocation checks exact presentation, revision and displayed
membership. Snapshot-count retention follows the last encoded-byte reader.
Background reads carry both the publication predecessor and action admission
epoch. An action blocks refresh publication until all admitted actions finish;
superseded reads coalesce into one trailing refresh. Candidate encoding runs
outside the publication lock. Membership changes and scoped model invalidation
have distinct owners, so unrelated presentations do not refresh together.
Portable UI receives an explicit target-scoped business API capability through
an ordinary child Profile; opaque presentation strings grant no domain access.

Authenticated UI API bindings derive Session authority server-side. Input tickets
are consumed before UI admission and never automatically replay mutations after
an unknown outcome. The Worker consumes these remote models through the same
presentation-only document ABI as local contributions.

One resident HttpAssets object publishes renderer-only candidates by expected
generation. Current, retiring and candidate graphs share one budget. Full graph
leases retain old lazy imports; unchanged file bytes reuse existing reservations.
Document mount/update/dispose and old-work drain finish before acknowledgement.
One document owner spans Worker replacements and closes replacement admission
synchronously. Disposal rejection or deadline expiry requires a document reload,
because a new table cannot prove that arbitrary old renderer resources are gone.
Both bridge ends reserve bounded lifecycle admission independently of ordinary
input, while draining admitted mutations before reporting shutdown.
The native terminal owner retains TTY modes, hooks, controller and drafts while
an ordinary presentation subtree loads independent native renderer code.

## Alternatives considered

Executable URLs in model data mix domain data with code admission. Per-candidate
budgets hide aggregate retention, and response-only pins lose unfetched modules.
A generic durable action ledger duplicates domain receipts. A second JavaScript
business runtime duplicates Worker ownership. Native ABI pointer tables are not
a browser/WASM loading interface.

## Consequences

Real PTY native A/B replacement preserves the pending model request and draft.
Chromium and Firefox exercise dynamic DOM replacement, asynchronous cleanup,
failed-candidate retention, lazy imports, and Rust/WASM DOM ownership. A real
native UI fixture calls a scoped Session API and supplies a remote arbitrary
model and binary source to the Worker/Rust renderer. Live DeepSeek coding covers
TUI completion and Web continuation of the same durable Session.

Native callbacks and document renderers are trusted cooperative code. Browser
ESM imports cannot be unloaded, so each document admits at most 32 attempted
renderer generations; reconnect does not reset that count. A failed candidate
may leave the old displayed graph occupying the sole retiring slot until its
observer is released. A 64 MiB cold graph need not fit a hot overlap. Encoded
budgets do not bound total RSS or browser decoder storage. Worker/bootstrap
code changes require restart; a Worker WIT business loader remains separate work.
