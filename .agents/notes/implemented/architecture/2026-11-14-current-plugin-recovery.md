---
name: Current-plugin recovery and bounded management lifetimes
---

## Problem

Historical executable recipes and Store-owned reference codecs couple complete
Session history to obsolete plugin code. Failed candidates consume codec slots.
Management previews and completed receipts retain capacity until restart, while
their recovery guarantees exceed the product's present needs.

## Decision

This supersedes the historical executable recovery and full-state publication
clauses of [durable generation selection](2026-11-12-durable-agent-generation-selection.md).

Sessions resume with the current composition and validate retained state at their
owning plugin boundary. Immutable pins retain already admitted work without
retaining executable recipes for historical restoration. Validated durable
references accompany canonical Domain state so maintenance does not execute old
code. Generation cuts append only changed Domain states and explicit removals.
Cold admission validates private plugin state before issuing a resume token.
Publication transfers the existing submission lease and hydration ownership to
the owned commit task until resident installation completes. Keeping only the
Store write alive would let cancellation admit another cold pin at the same cut.
Published composition caches retain one eagerly constructed pin; source probes
compare exact process-local catalog identity, while durable hashes exclude Rust
nominal type identity.

Historical output views use the Store's existing append Fact anchors to fold
generation deltas through the executing intent. Persisting duplicate schemas in
every Tool result or selecting the latest catalog would add storage or mislabel
old results. Pure Host fingerprints encode frozen declarations rather than
compiling an artificial empty root, which cannot validate root-owned patches.

Management previews are stateless. Commit revalidates the supplied change and review
digest, returns the operation result directly, and retains no
completed receipt. Short-lived work is bounded independently for Local, Device and
Agent callers, with serialized publication and no per-principal scheduler.

Changed-builder Context loads incrementally with cancellation and bounded reads,
without repeatedly abandoning valid long histories at an aggregate deadline.
Relevant watermarks and batched Store checks avoid additional caches.
Concurrent source probes share only their in-flight work. Boundary freshness
still checks bytes: mtime or a cache lifetime cannot establish that an evaluated
Profile is unchanged. Context rejection is remembered for the Turn because
ordinary Fact progression must not cause repeated failed full-history replays.
Capacity pressure is not evidence of incompatible state and remains retryable.
External mirror cleanup survives caller cancellation and retains its Session load
lane until abort settles. Uncertain business operations and dropped application
views reserve bounded ownership before dispatch and retain it through settlement;
execution concurrency alone cannot bound accumulated historical cleanup work.
Preparation timeout and source failure before candidate identity is available
also defer retry to the next Turn, including a source edit made after failure;
before source identity is available, a narrower retry key
cannot distinguish a repaired candidate from the same stalled probe. The
[generation contract](../../../../crates/rsi-agent/docs/subsystems/composition-generations.md)
owns the exact retry and cancellation boundaries.

## Alternatives considered

Acknowledged receipt retirement and provisional codec leases preserve stronger
historical recovery, but require permanent ownership protocols. The product
instead adopts the lighter lifecycle demonstrated by DSH's current-composition
resume and nonretained completed plugin-management results. DSH does not provide
an equivalent Domain-codec garbage-collection protocol: canonical reference
envelopes are an RSI storage decision. Fair per-principal scheduling and retained
cold pins add state and resource lifetimes without a required product guarantee.
Keeping an executable-recipe decoder without a production consumer would preserve
another durable-input boundary and redundant validation. Profile instead hashes
the already validated evaluated tree directly. It retains neither encoded recipe
bytes nor a restore API; exact JSON numbers, node order and isolation still affect
the diagnostic identity.

## Consequences

Repeated failed and successful plugin changes do not exhaust historical codec or
management-result registries. Cold recovery uses current plugins, refuses
incompatible retained state, and never replays completed effects. Maintenance
and garbage collection work without unloaded plugins. Changed-state history,
revision fencing, forks and purge agree across Memory and SQLite Stores.
Targeted deterministic, visual and opt-in live checks cover the changed surfaces.


History remains complete and can grow until explicit purge. Current plugins may
not interpret historical custom data; old executable recovery is not promised.
Offline maintenance verifies canonical references and indexes, not the business
visitor semantics of an unloaded plugin. A lost management reply remains unknown;
current desired/applied state cannot prove the original operation's outcome.
Class capacity does not guarantee fairness among competing members. Old Store
formats are explicitly rejected rather than silently migrated or reset.
