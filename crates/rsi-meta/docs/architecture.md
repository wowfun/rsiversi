# rsi-meta architecture

The foundation has one composition graph and one lifetime graph:

```text
Runtime
  └─ persistent Context
       └─ apply ResolvedFactory
            └─ Fiber generation
                 ├─ prepared Local and Portable requirements
                 ├─ dynamic Local and Portable supplies
                 ├─ listeners and child Fibers
                 ├─ transferable capabilities
                 └─ reverse-ordered effects
```

No product registry, factory catalog, Profile loader, native branch, daemon, or
persistence mechanism is embedded in core. `PluginFactory` contains execution
behavior; callers resolve immutable identity and update policy before applying
it. A product authority is a plugin Fiber; a passive leaf value is an
effect-owned contribution to that authority.

## Context and ownership

A `Runtime` owns all mutable registries, admission, scheduling, resource
accounting, persistent cleanup, and shutdown. A `Context` is a cloned
capability value that retains its Runtime and optional owning Fiber generation.
Lane-specific isolation and Portable call trace derive child contexts without
changing their parent. Product scope is carried by an explicit wrapper above
core; Context has no generic extension store or configuration intercept map.

Every structural mutation validates the Context's Runtime, Fiber, and
generation at its linearization point. A stale Context can be inspected but
cannot publish, open a call, register an effect, or create a child. Root
Contexts can apply root plugins but cannot impersonate a plugin generation.

## Preparation, injection, and activation

`ResolvedFactory` contains one already bounded `FactoryIdentity`, static
`UpdateMode`, and the `PluginFactory` implementation. The Runtime validates and
accounts the captured identity for the Fiber lifetime but never asks executable
plugin code to report it. The Runtime validates each desired
configuration at its owning input boundary, then retains that bounded value as
the proof reused by later attempts. Configuration numbers use the workspace's
exact JSON representation, including decimal text that binary floating point
cannot reproduce. Every preparation borrows that unchanged
desired value without repeating boundary validation; it never receives a
previous attempt's normalized output. Plugin-returned normalized configuration
is independently validated before retention. Preparation has no generation
Context and cannot read the services whose requirements it is deciding. It
returns one `PreparedActivation` containing that attempt's
normalized activation configuration, exact requirements, and at most one opaque
`Send + 'static` state value. Configuration and requirements are immutable
after preparation. An unapplied `PreparedPlugin` remains external admitted
ownership: it retains its pessimistic Fiber and attempt reservations until it
is consumed or dropped, so shutdown cannot report zero resources while a proof
is still live. The state has one owner and can be taken successfully at most
once; a wrong-type take preserves it. Its declared byte charge remains owned by
the attempt until that attempt retires, including after activation takes the
value, because core cannot observe whether the plugin moved it into
generation-owned state. This is deliberately conservative rather than a claim
of byte-exact early release.

The Runtime resolves exact hard Local and Portable requirements from one
registry revision. Only an actual supply owned by an Active provider generation
can satisfy an external requirement. Missing supplies leave the Fiber
`Pending`; diagnostics report a bounded prefix of missing actual requirements
and failures. Optional Local lookup is point-of-use discovery and never becomes
a dependency edge. Portable has no optional discovery.

When all requirements resolve, the Runtime constructs one `ActivationPlan`
with capabilities minted directly from the exact resolved supply bindings plus
the prepared state. Before entering plugin code it allocates one nonzero
`CallId`, installs it on the activation Context as the root lineage with the
current Fiber as origin and no parent, and exposes it through
`ActivationPlan::lineage_call_id`. Exhaustion fails closed before `activate` is
called. A prepared activation is single-use. A desired-revision or
binding-identity change fences a stale Loading attempt, rolls it back, and
starts the next attempt with fresh preparation. Exact injected bindings are
revalidated under the registry lock both when Loading ownership is installed
and at final publication, so a withdrawal cannot fit between snapshot
resolution and cancellation-token installation. Loading installation also
requires the exact resolved attempt and desired revision to remain current and
rejects a concurrently requested disposal. Reconfiguration and disposal use the
same registry-to-Fiber lock order for replacement or cancellation, so neither
can miss the installation boundary. Successful activation commits
its setup transaction and becomes `Active`; commit retains effects but is not a
separate registry-publication operation.

One private generation-activation owner contains the exact resolved-attempt
Loading install, generation-root setup transaction, rollback, and final
registry publication as one lifecycle operation. Its reconciliation-facing
interface accepts the Fiber plus the exact resolved binding proof; callers do
not reproduce any publication fence. A narrower activation driver contains
plugin future construction, polling, cancellation-time and normal future
destruction, prepared-state destruction, and caught panic-payload destruction.
The generation setup authority remains live through that teardown. No
plugin-owned prepared value is destroyed while a Runtime registry or Fiber-data
lock is held.

Reconfiguration validates, reserves, and prepares the replacement desired value
before it changes the Fiber's installed revision or retires an Active
generation. While staging, the old desired value and active attempt coexist with
the replacement desired value and prepared attempt, and every distinct retained
allocation and requirement edge remains charged to its owner. Preparation or
reservation failure discards only the replacement and leaves the old revision,
watchers, and Active generation unchanged. Installation atomically replaces the
desired revision, pending attempt, and requirement watchers; stale requirement
slots can no longer wake the Fiber.

Apply, supply changes, reconfiguration, disposal, and shutdown converge through
one Runtime-owned bounded scheduler. A Fiber has at most one active transition
and one coalesced queued intent. Ready work is disjoint from active work; a new
request for an active Fiber enters a separate one-entry rerun frontier and is
promoted to the ready tail only when that exact transition finishes. Ready
selection is FIFO, so a repeatedly requested Fiber cannot overtake work that
was already waiting, and takes a ready Fiber directly instead of rescanning
active IDs. A transition yields its
global scheduler slot before joining nested Fiber work, waiting for
preparation capacity, or draining admitted capability uses, and reacquires it
before local mutation. External
preparation remains fail-fast, while an already-admitted Runtime reconciliation
waits for transient preparation pressure rather than terminalizing a healthy
generation or spinning a retry intent.
Caller cancellation or deadline expiry detaches only the waiter; admitted
preparation, activation rollback, retirement, and shutdown remain owned and
joinable.

## Transactional effects

Every activation attempt begins with an `EffectTxn` record already installed
in the Fiber's cleanup ownership. Plugin code never runs in the gap before the
wrapper exists. `defer` appends a bounded exact undo; explicit effect disposal
and Fiber retirement claim the same idempotent record. Cleanup runs last-in,
first-out and continues after returned errors, cleanup unwinds, and caught
panic-payload destructor unwinds while retaining bounded evidence. Cleanup
invocation and caught payload destruction use separate unwind boundaries, so a
hostile payload cannot skip sibling undos.

An open transaction that errors, panics, is dropped, or races unload is aborted
by Runtime-owned work. Unload joins setup and rollback rather than skipping an
in-flight wrapper. Once unloading begins, creating a transaction and committing
one fail. The original owner of an already-open setup may still defer the exact
undo it acquired while unload was claiming the wrapper; abort, Drop, or the
failed commit then closes the setup window. Closed and stale transactions reject
further mutation. The transaction can reverse only mutations whose undo was
successfully registered; code that performs an external side effect before
registering cleanup remains responsible for that unowned interval.

`InvocationContext::caller_effect` lets a service implement an operation on
behalf of its exact caller generation. Contributions made through that handle
retire with the caller, not the provider. The handle is generation-fenced and
cannot register after its owner transaction closes.

## Local and Portable supplies

The two contract lanes share Fiber effect ownership and dependency convergence,
but not their call interfaces.

`Context::provide_local<C>` registers one safe-Rust object in the exact
`(TypeId, LocalIsolationId)` slot and returns a `LocalSupplyId`-fenced disposer.
`PreparedActivation::requiring_local<C>` creates a hard managed edge;
`Context::lookup_local<C>` observes only an Active supply and creates no edge.
One exact slot has one provider. Withdraw and re-provide in the same generation
allocate a new supply token, fence stale removal, and replay exact hard
consumers. Escaped `Arc` values remain ordinary caller-owned Rust objects and
are not revoked or drained by Runtime.

Portable `Context::provide` remains the authority for a serialized endpoint.
It claims one isolated slot and returns a `SupplyId`-fenced disposer.
`Context::provide_and_capture` additionally returns the provider generation's
own `Capability`. It reserves and registers that capability before the supply
can enter the registry, so any capability failure leaves no supply to observe
or withdraw.

A Loading supply occupies its slot immediately and is available to its own
providing generation, but it cannot satisfy external lookup, injection, or
call opening until the provider is Active. This exception prevents a dependent
activation from becoming part of an uncommitted provider transaction.

Listeners, transferable capabilities, and product contributions are visible
while their owner is Loading because their exact undo is already installed.
Activation failure therefore permits a bounded visible-add/visible-remove
sequence and then awaits deterministic rollback.

Active add and withdrawal notify only consumers of the complete lane-specific
slot and exact supply identity. Withdrawal removes external visibility and, in
the same registry transaction, fences every exact dependent Loading attempt and
queues its reconciliation. Local withdrawal joins that convergence and then
drops Runtime ownership without waiting for escaped objects. Portable
withdrawal additionally drains calls admitted before closure. No notification
is derived from a declaration.

A dormant supply cleanup retains its exact owner, slot, binding, executor, and
resource reservations, but only a weak Runtime reference. The Runtime already
owns that cleanup through the generation effect record; letting the cleanup
retain the Runtime would create a structural last-owner cycle. Once disposal
starts, its Runtime-owned task upgrades and strongly retains the Runtime until
withdrawal and result publication finish. If the Runtime has already ceased to
exist, dropping its owned state has already withdrawn the registry and the
dormant cleanup completes without attempting reconciliation.

## Messages and capabilities

The universal call value is `Message { bytes, capabilities }`. One queue
transaction admits the destination channel position, byte weight, and
capability references together; all three remain owned while queued and are
released when the Message is consumed or dropped. A sender that cannot yet
enter that transaction consumes one independently bounded pending-send
reservation and holds none of the three queue resources. Pending senders are
keyed for logarithmic removal. Each channel exposes a constant 65-entry
candidate window to the mixed-weight scheduler; registering or cancelling a
nonfitting waiter does not rescan an unchanged global candidate set, while
removing a fairness barrier or exposing a newly fitting channel candidate
resumes scheduling. A newly registered fitting waiter displaces the youngest
nonfitting candidate when that channel's window is already full, so the
constant window cannot hide usable capacity. Minting owns one
Runtime-wide capability entry; cloning or transferring a safe-Rust handle
shares that entry and does not mint or register another authority. A capability
is an opaque Runtime- and generation-fenced possession authority; safe Rust
exposes no raw token, reconstruction, import operation, kind, or rights
metadata. Native ABI capability IDs belong to independent Loader-owned adapter
tables. A core capability is not an implementation pointer or a product schema.
Its safe diagnostic form shows only bounded logical service and provider facts.
Generation retirement revokes use and removes the entry from its generation's
revocation set, but a live safe handle continues to own its unique entry
reservation until its final clone or containing Message drops. This prevents
repeated mint-retire-retain cycles from bypassing the Runtime memory bound. A
clean shutdown therefore requires callers to release every capability they
still own; stale possession is bounded state, not an unaccounted tombstone.
An adapter that must retain possession without retaining Runtime lifetime
consumes the handle into `DetachedCapability`. It keeps the exact entry and its
resource charge plus a weak snapshot of the original holder scope, excluding
activation setup authority. `upgrade` can reconstruct only that original holder
while its Runtime still exists; it cannot rebind authority to another Context.
This breaks structural adapter ownership cycles without freeing stale capacity,
forging a tombstone, or weakening generation fences.

`Capability::open` returns one deadline-bound bidirectional call.
`Capability::invoke` is an exact unary adapter: one request, request-side
close, exactly one response, then clean terminal. Zero responses, a second
response, provider error, panic, timeout, cancellation, or an absent terminal
cannot be reported as unary success.

One Runtime-owned driver retains the caller generation, provider generation,
channel halves, queued-message accounting, cancellation, deadline, and unique
terminal. Safe-Rust providers receive a borrowed channel that cannot escape the
callback lifetime. Receiving or dropping a Message releases its queue
reservation; observing the terminal destroys the caller inbox and releases
late queued responses. The bounded terminal result remains sticky on the public
call: subsequent reads repeat its error, while only a clean terminal is EOF.
`CapabilityCall::cancellation_observer` returns a cloneable observation-only
view of that exact call's cancellation fact. It remains valid while an adapter
temporarily transfers the caller half to blocking or foreign execution, but it
cannot request cancellation or expose the underlying token. Provider callbacks
receive the same observation-only surface.
At serialized fail-fast adapter seams, unrelated contention and same-lineage
recursion remain distinct typed terminals: `MetaError::Busy` and
`MetaError::Reentrant`. Adapters preserve that distinction rather than
recovering authority semantics from diagnostic text. Core safe-Rust provider
callbacks are not implicitly serialized by this adapter rule.

Every Portable service callback receives three distinct call facts. `call_id`
names that callback, `parent_call_id` names only its immediate enclosing call,
and `lineage_call_id` names the activation seed for the complete chain. The
first callback opened from an activation Context therefore has a distinct
`call_id`, no parent, and the plan's lineage. A provider Context carries that
lineage and its current call into subsequent service calls, so
arbitrary re-entry preserves one root identity without thread-local or global
tracing state.

## Local events

Each `LocalEvent` marker fixes its argument, output, and one of the five dispatch
modes in its type. Callers dispatch the marker and cannot select a mode at the
call site. Emit is synchronous ordered fail-fast; Parallel is asynchronous
all-settled with aggregate errors; Serial is asynchronous ordered first-break;
Bail is synchronous ordered first-break; Waterfall is a synchronous typed
continuation chain. Because Waterfall's around/short-circuit contract requires
nested continuations, each exact Waterfall event slot has its own configured
listener ceiling in addition to the Runtime-wide listener ceiling. Runtime
construction rejects a per-slot ceiling above the hard stack-safety bound of
256 listeners.

Listener registration is an immediate effect-owned mutation, including while a
generation is Loading. Append is default; prepend and atomic once are explicit.
Registration, once claiming, explicit disposal, activation rollback, and Fiber
retirement share one generation-fenced removal transaction.

Dispatch snapshots exact `(TypeId, LocalIsolationId)` listener bindings and
releases registry locks before callbacks. There is no target callback, ancestor
selection, or global bypass. An ordinary binding already present in the
snapshot remains eligible for that dispatch if its handle is concurrently
disposed; disposal removes it from future snapshots. A once binding still
requires its exact atomic claim and cannot run twice across concurrent
dispatches.

The registry preserves append/prepend order with stable internal ordering keys;
exact-handle removal uses the listener identity's indexed location and never
shifts the remaining slot membership.

Local callbacks execute directly. Runtime does not add a common deadline,
spawn, cancellation token, call identity, or dispatch resource tracker. Errors,
panics, and typed breaks follow ordinary safe-Rust semantics and the marker's
mode. Parallel constructs one caller-owned all-settled Future from the exact
snapshot, preserves listener order in its aggregate errors, and polls at most
64 callback futures concurrently. A Parallel once binding is claimed only when
that dispatch admits its callback into the polling window; dropping the Future
does not consume bindings that the window never admitted.

## Scoped contributions

`rsi-meta-scope` is a library above core, not a Runtime service. The retired
generic `ContextExtension` metadata facility was removed without migration to
Local contracts; product-specific immutable metadata belongs in an explicit
typed wrapper or owning module. A
`ScopeRoot` is constructed with an explicit maximum complete ancestry depth in
the inclusive range `1..=4,096`; the key itself counts toward that depth. The
root mints opaque `ScopeKey` identities and serializes parent-link changes, but
it does not retain a registry of minted keys. Each live key owns its parent
link and non-owning child links support depth validation without retaining
descendants. Bind and rebind reject an edge before it would make any key in the
moved subtree exceed the root's configured depth. A child that has never been
a parent cannot occur in a proposed parent's ancestry, so ordinary leaf
attachment performs no ancestor walk; the monotonic proof is discarded only
by dropping that key. A possible cycle is still checked to the root. A child
or parent binding retains only the ancestry it can still observe, and otherwise
dropping the last key iteratively reclaims the complete unreachable chain
without a root-side sweep, recursive final destruction, or historical-key
bound.
`ScopedContext { Context, ScopeKey }` is the explicit product-facing wrapper.
Scope-owned registrations use its owning Fiber effects; scope never becomes a
generic Context extension and does not participate in Local event targeting.

`ScopedLayers` owns one eager global layer and lazy exact-scope aggregate
layers. Effective named snapshots apply global values, then ancestor overlays
from farthest to nearest; nearest same-name values win without moving unrelated
entries. Overlay resolution retains shared entry values and clones only the
final visible owned snapshot. Exact `peek` validates only root identity and does not walk ancestry;
reads never create a layer. `NamedEntries` and
`AnonymousEntries` preserve insertion order and exact independent ownership.
Each product store declares its maximum simultaneously retained exact-scope
layers. An existing key remains usable at saturation, while a new key fails
before factory execution; a cleanup failure may consume capacity but cannot
turn repeated distinct-key churn into unbounded retained history.
Each layer's reclamation ABA version saturates instead of wrapping. The layer
continues to accept capacity-bounded mutations after exhaustion, but that exact
slot permanently fails closed against automatic reclamation.
Failed lazy materialization removes its exact uninitialized cell before
returning, so a panicking layer factory cannot retain the scope key or create
root-like history in the product store.

The original `ScopeParentBinding` is the only rebind authority. Rebind checks
same-root identity, subtree depth, and cycles atomically but neither proves
quiescence nor notifies product registries. A product that retains derived
state owns that precondition and notification.

Layer mutation and visibility derive from one Context. Add becomes visible
before the fallible change callback. If that callback fails, exact undo and a
compensating callback follow; the first failure remains authoritative. Removal
never resurrects an entry when its notification fails. User callbacks never
run while a store lock is held, and reads return owned snapshots. Every caught
lazy-factory, action, change-callback, undo, or reclamation panic also destroys
its panic payload behind a second unwind boundary. A panicking payload
destructor becomes bounded failure evidence and cannot escape or skip the
remaining exact undo, reclamation, or notification path. Change futures are
both polled and explicitly destroyed inside that containment before their
result is published. Their owner guard applies the same destruction boundary
when the mutation waiter is cancelled while polling; the dropped open
`EffectTxn` still transfers exact undo and notification to Runtime-owned abort.
Built-in entry stores publish each new exact undo to the surrounding action
transaction before returning it to product code, so an action error or panic
after insertion cannot strand an unowned visible entry.

## Native adapter

The native path remains an adapter chain:

```text
explicit path -> verified staged mapping -> ABI v3 Portable capability port
              -> ResolvedFactory -> ordinary Fiber
```

ABI v3 carries bounded Messages, capability ownership, exact Portable contract
metadata, dynamic provide, and setup effects across one exchange port. The
native loader maps foreign handles into the same core authorities used by
Portable Rust adapters; it does not introduce a second declaration registry or
lifecycle model. Callback-bound channel and effect authority cannot become
durable product state. The maintained
[`rsi_meta_plugin.h`](../native/include/rsi_meta_plugin.h) owns the exact entry,
version, frame, opcode, status, and one-shot release contract.

Native code is trusted process code selected by explicit artifact path, not a
package or sandbox. Host computes and records the exact staged-byte digest on
start or requested reload; the artifact path is not watched. A create or call timeout
terminalizes its Runtime, but the callback frame, thread, mapped library,
capabilities, cache lease, and accounting remain retained until foreign code
actually returns. Loader admission is fail-fast and preserves callback lineage
across nested calls so native callbacks cannot turn serialization into a
deadlock. The [Loader contract](../native-loader/README.md) owns cache, finalization,
and platform details.

## Retirement and shutdown

Retirement closes external admission, withdraws Active supplies, converges
dependents, drains admitted calls and dispatches, disposes children, and then
runs effects in reverse order. Cleanup is idempotent and joinable; dropping a
waiter does not drop the work. User cleanup panic is contained as a bounded
failure. A panic escaping ownership machinery terminalizes the Runtime because
registry withdrawal is no longer provable.

Shutdown first closes Runtime-wide external admission, captures strong root
Fiber ownership, cancels call and dispatch drivers, and starts every root
disposal. It hard-seals retiring admission only after convergence, then drains
pre-close leases. `Complete` requires an empty Fiber registry, idle scheduler,
zero logical resources, and a sealed and drained gate. A deadline bounds one
wait and returns tracked unresolved evidence; it never authorizes abandoning
cleanup.
