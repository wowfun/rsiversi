---
name: Cordis capability foundation
comment: Dynamic Fiber composition, transactional effects, scoped contributions, and native ABI v2
---

## Problem

The previous foundation made static plugin declarations authoritative for
dependencies and provisions, staged every setup mutation until publication,
and exposed only byte-oriented services. Product registries consequently needed
either privileged core support or unrelated lifetime systems, while native
plugins followed a second lifecycle model that could not carry capabilities or
transactional setup ownership.

The foundation instead needs to derive composition from resources that actually
exist. Plugin setup must own its undo before publishing state, service identity
must not depend on speculative declarations, and reusable scope mechanics must
not turn Runtime into an untyped global registry. Native adapters must preserve
the same ownership and call model across the C boundary.

This decision supersedes the static declaration, staged publication,
byte-service, and native ABI v1 parts of the
[Runtime-owned plugin foundation](2026-08-22-runtime-owned-plugin-foundation.md).
It retains that decision's single `Runtime -> Context -> Fiber` lifecycle,
bounded persistent work, generation fencing, and trusted-native boundary.

## Decision

### One composition authority

`Runtime -> Context -> Fiber` is the complete composition model. A Fiber is one
independently converging, long-lived plugin authority. Mutable product policy,
providers, adapters, and registrars are plugins. Leaf tools, commands, prompt
fragments, hooks, and similar values are effect-owned contributions to their
product plugin rather than Fibers by default.

Core exposes no generic `RegistryStore`, arbitrary named-value wire, or product
registrar. Product plugins own validation, duplicate policy, ordering,
filtering, schema, wire format, and observation. The independent safe-Rust
`rsi-meta-scope` crate reuses only scope identity, layered storage, snapshot,
and effect-ownership laws.

### Preparation, activation, and actual dependencies

`PluginFactory` has three adapter-neutral operations:

1. `identity` returns bounded diagnostic identity.
2. `prepare` validates desired configuration without a generation `Context` or
   injected service. It returns one `PreparedActivation` containing immutable
   normalized configuration, exact requirements, and at most one single-owner
   opaque state value with an explicit conservative retained-byte charge.
3. `activate` receives an `ActivationPlan` with the exact resolved
   generation-fenced capabilities, normalized configuration, one activation
   lineage, and the single-use prepared state.

Each activation attempt prepares afresh from the retained desired value. A
binding or desired-revision change fences a Loading attempt; prepared state is
never activated twice or moved to another Runtime. Reconfiguration prepares a
replacement before retiring the active generation and accounts every coexisting
desired, normalized, requirement, identity, and opaque-state value.

Static `requires` and `provides` declarations and the declaration index do not
exist. Pending diagnostics report unresolved actual requirements. Without an
actual supply there is no dependency edge, so mutually intended but absent
providers remain honest missing-supply reports rather than speculative cycles.

One bounded Runtime reconciliation scheduler coalesces revisions. Nested
lifecycle waits and internal preparation-capacity waits yield scheduler
admission. External preparation remains fail-fast, while transient pressure on
an already-admitted reconciliation is neither plugin failure nor a reason to
retire the installed generation. Cancellation never abandons admitted
preparation, rollback, retirement, or shutdown.

### Wrapper-first effects and dynamic supplies

Before plugin activation, Runtime installs one generation-root `EffectTxn` in
the Fiber's cleanup ownership. Loading `defer`, dynamic supply, listener, and
product contribution operations all append to that same root. Every helper
installs exact undo before returning observable state. Commit closes setup and
retains reverse-ordered effects; it is not a publication operation.

Explicit disposal, open-transaction Drop, rollback, and Fiber retirement claim
the same idempotent effect records. Started asynchronous cleanup becomes
Runtime-owned and remains persistent if its waiter is dropped. Cleanup failures
are bounded evidence and do not skip later LIFO effects. A panic escaping the
ownership machinery terminalizes Runtime when consistent withdrawal can no
longer be proved.

`Context::provide` creates a non-repeating, generation-fenced `SupplyId` and
returns a `SupplyHandle`. A Loading supply occupies its complete key/isolation
slot and is visible to its own provider; external lookup and injection require
the exact provider generation to be Active. Withdrawal removes visibility,
cancels affected Loading attempts, and queues their exact reconciliation in one
Runtime-state transaction before user-owned destruction. It then converges the
captured dependents, drains calls admitted before closure, and drops the
endpoint. Replacement compares complete supply identity, so an old handle
cannot remove a new same-generation supply.

`Context::provide_and_capture` additionally returns the provider's own
capability. Capability admission and generation registration precede supply
publication, making the operation atomic. Dormant supply cleanup holds only a
weak Runtime reference to avoid a Runtime-owned effect cycle; starting cleanup
synchronously upgrades that reference and gives the complete withdrawal task a
strong Runtime owner.

Listeners, transferable capabilities, and product contributions are visible
during Loading because their exact undo is already installed. Services remain
Active-gated because binding a dependent to an uncommitted provider would merge
two setup transactions without a common owner.

`InvocationContext::caller_effect` is observation-limited authority to append
cleanup to the exact caller generation. It cannot be retargeted and becomes
stale when the callback or caller generation closes.

### Messages, capabilities, and calls

The universal call value is `Message { bytes, capabilities }`. Channel entry
atomically admits one destination position, byte weight, and capability
references under per-message, queue, and Runtime bounds. Waiting senders own a
separate bounded reservation without partially owning that transaction.
Pending senders use keyed removal, each channel contributes at most 65 ready
candidates, and unchanged nonfitting candidates are not rescanned during
registration or cancellation. These structural units, tested at the default
65,536-waiter scale, are the deterministic complexity gate; fixture wall-clock
measurements remain diagnostic only. A
safe-Rust `Capability` is opaque possession of one generation-fenced service
binding. Minting creates one accounted entry; cloning and message transfer
share that entry rather than minting authority. Safe Rust exposes no raw token,
reconstruction, arbitrary import API, kind, or rights metadata.

Generation retirement immediately revokes use and removes the entry from its
generation's revocation set. A retained safe handle continues to own its unique
entry reservation until its final clone or containing Message drops, preventing
stale-handle churn from bypassing the memory bound.

Adapters that must retain possession without retaining Runtime consume a
capability into `DetachedCapability`. It preserves the exact entry, charge,
generation fence, and original holder scope with a weak Runtime reference and
without setup authority. `upgrade` can reconstruct only that original holder
while Runtime exists; it cannot rebind authority to another Context.

`Capability::open` creates one deadline-bound bidirectional call.
`Capability::invoke` is an exact unary adapter: one request, request-side close,
exactly one response, then a clean terminal. Zero or extra responses, provider
failure, panic, timeout, cancellation, or an absent terminal are not success.

The Runtime-owned call driver retains both generation leases, channel halves,
queued-message accounting, and the unique terminal. `CancellationObserver`
lets a blocking adapter observe that exact call's cancellation while the movable
caller half is elsewhere, but exposes neither cancellation authority nor the
underlying token.

Every activation has a nonzero lineage seed. Service and event callbacks expose
their own `call_id`, immediate `parent_call_id`, and unchanged
`lineage_call_id`. At serialized fail-fast adapter seams, same-lineage recursion
and unrelated contention remain distinct typed outcomes, `Reentrant` and
`Busy`; core safe-Rust provider callbacks are not implicitly serialized.

### Context extensions, events, and scoped contributions

Typed `ContextExtension` markers key immutable copy-on-write local metadata.
Two markers may carry the same Rust value type without aliasing. Extension
contents are safe-Rust `Send + Sync + 'static`, redacted from diagnostics,
unserialized, and absent from the native ABI.

Event dispatch accepts an `EventTarget` that selects against immutable listener
Context views outside Runtime locks. Global listeners bypass selection. A
selector error or panic starts no callbacks. Synchronous selection runs on
blocking work bounded by dispatch admission and is covered by the same absolute
dispatch deadline as callback admission and execution. If a selector outlives
that deadline, the caller detaches but its worker retains Runtime admission,
the dispatch reservation, and the snapshot until return; the expired dispatch
cannot start later selectors or callbacks. Snapshot membership survives
concurrent ordinary removal for that dispatch, while once listeners still need
an exact post-selection claim.

`rsi-meta-scope` owns `ScopeRoot`, weak-key `ScopeKey` ancestry,
`ScopeParentBinding`, `ScopeTarget`, `ScopedLayers`, `NamedEntries`, and
`AnonymousEntries`. A root retains no minted-key inventory; live descendants or
bindings retain only their observable ancestry. Rebind atomically checks depth
and cycles and moves the original binding, but product code owns quiescence and
notification. A root has its own explicit validated ancestry-depth limit because
scope keys can cross Runtime contexts and therefore cannot inherit one Runtime's
Fiber-depth configuration. Non-owning child links make that bound enforceable
for a moved subtree without creating a root registry. A monotonic
has-ever-been-parent fact proves that attaching an ordinary new leaf cannot
close a cycle; possible cycle closure still walks and rejects the parent chain,
and final uniquely owned ancestry is dismantled iteratively. `ScopeTarget`
captures one old-or-new ancestor-chain snapshot and precomputes its membership
set so listener selection does not multiply ancestry work by listener count.

Layer mutations derive visibility and effect ownership from one Context. Add is
visible before the fallible change callback. If that callback fails, exact undo
and a compensating notification follow, with the first failure authoritative.
Removal never resurrects an entry when notification fails. User callbacks,
future destruction, reclamation, and panic-payload destruction run outside
store locks and inside nested containment.

### Native ABI v2 and Loader

Native ABI v2 is a clean break with one `rsi_meta_plugin_entry_v2` symbol and
one versioned exchange port in each direction. Fixed-width frames carry bytes,
message capability arrays, callback-local authorities, and explicit one-shot
release IDs. Every pointer, length, alignment, table prefix, issuer, slot,
epoch, kind, right, count, output, and operation phase is validated at its
owning boundary before mutation or dereference.

Owned output capabilities, callback-borrowed capabilities, and moved cleanup
capabilities have distinct ownership contracts. `HOST_EFFECT_DEFER` transfers a
cleanup lease only on success. Loader's single-owner `CleanupLease` schedules
the same ordered `RUN_CLEANUP` then `CAP_RELEASE` path when invoked, dropped
dormant, or returned as an unpolled future; rejected transfer performs neither
operation. The module FIFO retains that job ahead of factory finalization.

Callback frames seal activation, effect, provider-channel, and caller-channel
authority at foreign return. Native callbacks run on dedicated bounded OS
threads. Factory and instance gates linearize idle, busy, and poisoned as one
fail-fast admission state. One atomic callback outcome linearizes completion
versus timeout before gate reuse; foreign return alone cannot reopen admission
before that publication. Timeout poisons the adapter and terminalizes the
owning Runtime but retains the thread, frame,
instance, capabilities, mapping, cache lease, and accounting until foreign code
actually returns.

Plugin factory destruction is distinct from transport `FINALIZE`. Loader closes
host and plugin raw admission, drains earlier exchanges, runs cleanup and
instance destruction in one reserved per-module FIFO, destroys the factory, and
then attempts finalization. Successful finalization invalidates the table and
permits unmapping. Refusal, panic, or malformed success pins the complete bundle
and records the retained finalization rather than risking use-after-free.

Persistent host service slots store `DetachedCapability`, preventing a
`Runtime -> module -> HostTable -> Runtime` cycle. Core's dormant supply cleanup
is likewise acyclic. Direct last-owner Drop therefore reaches reserved native
destruction and finalization without requiring explicit Runtime shutdown.

The Loader host exchange remains one deep module because every opcode shares
admission, capability/output tables, callback sealing, effect phase, output
adoption, and failure encoding. The independent `module_teardown` module owns
the serialized destruction queue and fail-closed mapping bundle. Code-health
keeps the global 1200 effective-line hard limit and records the exact reviewed
Loader-region maximum rather than mechanically splitting one safety state
machine to preserve a v1 historical line count.

### Verification authority

`cargo xtask rsi-meta conformance` is the single local and CI orchestration
authority. It pins the queried rustc host triple, runs locked warning-denied
package checks and tests, validates standalone fixture metadata offline, runs
the foundation probe, builds and tests the real native fixture, verifies the
actual exported v2 symbol, and compiles the maintained header as C11 and C++17.
Repository documentation, Agent Note lifecycle, and code-health remain
independent owning gates rather than hidden conformance substeps. Snapshot or
baseline changes remain explicit contract changes rather than automatic gate
refreshes.

## Alternatives considered

Keeping static `requires` and `provides` beside dynamic supplies is rejected
because two composition authorities can disagree. Actual prepared requirements
and actual supplies are sufficient; static declarations add speculative state
and false cycle evidence.

Adding a Runtime-global generic registry is rejected because it makes product
validation and observation optional and creates an untyped wire whose lifetime
competes with capabilities. `rsi-meta-scope` reuses mechanics while each product
privately owns policy and schema.

Making every leaf contribution a Fiber is rejected. A Fiber represents an
independently converging authority; passive values belong to the effect
ownership of their product plugin.

Retaining atomic setup publication for all registrations is rejected because it
prevents setup-time hooks and forces every registry to duplicate staging.
Wrapper-first undo permits immediate bounded visibility. Services retain their
stronger Active gate.

Preserving ABI v1 through a compatibility adapter is rejected because byte-only
calls cannot faithfully carry capability possession, callback orientation, or
effect transactions. Pre-release status permits one v2 authority.

Strengthening scope rebind to prove quiescence or emit generic notification is
rejected because the scope primitive cannot observe product consumers or know
which derived views are durable. The product holding the binding owns those
guarantees.

## Consequences

Immediate listener and contribution visibility permits a bounded observable add
followed by rollback. Fallible notification cannot have a globally atomic
outcome; compensating notification exposes the final state when possible, and
bounded diagnostics retain failures without claiming isolation.

Removing static provider declarations removes declaration-derived cycle
diagnostics. Mutually absent providers are reported as missing supplies.
Scope creation is asynchronous and returns only after its ordinary child Fiber
is Active, rather than reproducing JavaScript's synchronous pre-activation
convenience.

Retained stale capabilities consume their accounted entry until the final safe
handle drops, so callers release their own possession before expecting a clean
shutdown. Started cleanup persists; dormant cleanup remains acyclic.

Native plugins remain trusted process code. The boundary contains Rust panics
and destructor panics but cannot contain aborts, memory faults, data races, or
arbitrary allocation. A recursively panicking payload can force retention of
one final payload. A plugin that refuses `FINALIZE` can force the Loader to pin
its mapping; safe unmapping or retry after ambiguous success is impossible.

The real dynamic-library, symbol, C11/C++17, timeout, teardown, and resource
evidence is executed on Linux. Portable layout checks and Linux execution do not
establish native Windows or macOS behavior; those hosts require their own CI or
local execution evidence.
