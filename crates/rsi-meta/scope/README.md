# rsi-meta-scope

`rsi-meta-scope` is the independent safe-Rust scope library above `rsi-meta`
core. It provides root-local opaque scope identity, parent-chain authority, and
product-composed layered contribution storage. Core does not depend on or
re-export this crate, and this crate does not add a generic Runtime registry.
The root never retains an inventory of minted keys: live keys and bindings own
only the parent chains they can still observe. Each root requires an explicit
validated complete-ancestry limit from 1 through 4,096. Topology mutation
enforces that limit for the whole moved subtree, ordinary leaf attachment does
constant ancestor work, and final uniquely owned parent chains are reclaimed
iteratively.

Scopes are asynchronously backed by ordinary child Fibers. Layer mutations use
the same Context for visibility and effect ownership, return owned snapshots,
and run product callbacks without holding scope-store locks.
`ScopeRoot::target` captures an atomic parent-chain snapshot, precomputes exact
membership, and returns the core `EventTarget` adapter used for one scoped
dispatch.
Failed lazy layer construction retains neither its uninitialized cell nor the
scope key, preserving weak-key ownership under repeated factory failure.
Each store has an explicit exact-scope layer maximum, so a layer that cannot be
proven empty may retain one slot but repeated distinct-key failures fail closed.
The per-layer reclamation version saturates; exhaustion permits later bounded
mutations but permanently disables automatic reclamation for that exact slot.
Caught scope callback panics also contain panic-payload destruction; bounded
failure evidence does not replace exact undo, reclamation, or notification.
Change futures are explicitly destroyed inside the same boundary before their
result is published, including when a Pending mutation waiter is cancelled.

The authoritative behavior contract is the
[scoped-contributions section](../docs/architecture.md#scoped-contributions).
Required public, lifecycle, failure, and concurrency evidence is specified by
the [scope test matrix](../docs/testing.md#scoped-contributions).
