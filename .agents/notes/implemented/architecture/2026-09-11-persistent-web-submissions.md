---
name: Persistent Web input and exact submission recovery
---

## Problem

A Worker can disappear while the user is editing or after a Session mutation was
admitted. Recovering only visible text loses canonical images and the original
mutation identity. Sharing a browser cookie across tabs also means an endpoint
identifier alone cannot identify the authenticated owner of saved input.

## Decision

The document owns bounded editable records in IndexedDB. Rust owns preparation,
execution and reconciliation of typed Session requests. An opaque Rust JSON
string preserves the complete prepared request across document reloads without
rounding integer revisions or changing command argument order. Separate editable
and pending revisions let typing continue while the original request settles.
Incarnation and revision comparisons protect every transactional write, including
atomic movement to an explicitly created replacement Fresh Session.
Recreation saves and holds the cached editor before it starts the replacement;
an older persisted value cannot stand in for unsaved local input. Conflict choices
remain available in the recovery list even when the old Session cannot reopen.

The connection API supplies the authenticated caller identity. Browser requests
pin that device identity and HTTP checks it against the current authentication
before dispatch or cookie removal. The document selects storage only after this
identity is established. The [Web contract](../../../../plugins/rsi/web/README.md)
owns current persistence, limits and recovery behavior; the
[HTTP contract](../../../../crates/rsi-api/http/README.md) owns the request fence.

Image references are not Session access grants. The authenticated
[Media API](../../../../crates/rsi-media/api/README.md) already reads canonical
objects at Service scope, and its reader validates the reference and returned
content. Restored document drafts may therefore preview a validated reference
without reconstructing a Worker membership list. The current pane generation
and exact-source ticket checks still fence their respective presentation paths.

## Alternatives considered

Persisting only editor strings loses image references and unresolved invocations.
Persisting browser-decoded Rust requests changes integers outside JavaScript's
exact range. A cross-tab execution lease cannot establish whether a vanished tab
already sent a mutation. Automatic command replay after an absent receipt would
repeat callbacks whose receipts are no longer available. A global cleanup flag
reset on reconnect cannot establish disposal of arbitrary renderer resources.

## Consequences

The prepared-to-dispatching transaction chooses one initial sender. An uncertain
message retains its identity and requires authoritative reconciliation before an
explicit exact retry; commands remain query-only after dispatch. Failed storage
or receipt persistence preserves local input and blocks a new submission identity.
Nonempty or unresolved records are never evicted to make capacity available.
Unavailable Sessions do not release frozen-request quota; enough unresolved
records can prevent further submissions in that pane across origin namespaces.
Adding a discard UI would surrender recovery evidence and needs an explicit
product contract, not an inference that a missing Session never executed work.

The dispatch marker remains set after a later definitive non-admission result.
It represents historical admission attempts, not confirmed transport delivery.
Resetting it from one rejection would need provenance for all earlier attempts
and successful draft commands; a single boolean cannot recover that information.

An expired Fresh Session can be replaced only when its saved record proves that
nothing was dispatched. Reattaching a still-live Fresh Session first reads its
typed draft snapshot instead of demanding nonexistent durable history. Historical
or uncertain Sessions retain their input without being silently recreated.

The expected-device fence cannot retract a Set-Cookie response that was already
authorized before another tab logged in. Browser cookie response ordering remains
outside that request-time guarantee. IndexedDB transactions establish local
commit completion, not protection from a compromised same-origin script or OS.
