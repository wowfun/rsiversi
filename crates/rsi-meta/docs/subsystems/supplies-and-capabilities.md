# Supplies and capabilities

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
