# rsi-meta

The public composition seam is `Runtime -> Context -> FiberHandle`. The crate
contains no unsafe code.

## Runtime

`Runtime::new` validates one policy grouped into topology, payload, execution,
and deadline limits. Accepted values are safe for every downstream channel,
semaphore, counter, JSON traversal, and deadline. `resource_snapshot` reports
logical retained ownership, high-water marks, and rejected reservations rather
than allocator RSS.

All public async operations require a Tokio runtime with time enabled. A Fiber
captures its executor at insertion so Runtime-owned transitions, rollback, and
cleanup remain schedulable when the initiating waiter is dropped on another
thread. Synchronous capability opening uses the captured executor and does not
require an ambient Tokio context.

`Runtime::snapshot` captures bounded registry membership under the Runtime
lock, releases it, and then observes individual Fibers. It is an operational
snapshot, not one global linearizable image. `Runtime::shutdown` is the
deterministic quiescence boundary.

## Context

A cloned `Context` retains its Runtime, owner generation, service-isolation
map, direct-edge intercepts, call trace, and typed extensions. Builders consume
the selected value and use copy-on-write storage, so cloned siblings remain
independent. Identifiers, entries, intercept JSON shape, and retained encoding
are validated before the new Context exists.

Typed extensions are safe-Rust values keyed by a marker type. They are inherited
by derived Contexts, may be shadowed without mutating siblings, and are not
serialized or passed through native ABI v2.

`Context::apply` inserts one child Fiber. Application preparation is
Runtime-bound, fail-fast, and capacity-reserved. The factory's per-attempt
`prepare` step borrows the Fiber's bounded retained desired configuration
without a generation Context and returns one single-use `PreparedActivation`.
That value owns only the current attempt's normalized activation configuration,
exact requirements, and at most one opaque safe-Rust state value. The factory
declares the state's retained-byte charge; this is a trusted typed-Rust resource
contract, while configuration and requirement metadata are measured by core.
Core conservatively holds that charge until the attempt retires, even after
activation takes the value, because the plugin may move it into
generation-owned state; there is no byte-exact early-release claim.
Every later attempt prepares again from the unchanged desired value, never from
the previous normalized result. Requirements are resolved into an
`ActivationPlan` containing exact injected capabilities and the single-use
state. Before plugin entry, core allocates a nonzero call-lineage seed, installs
it on the activation Context with the current Fiber as origin and no parent,
and exposes it as `ActivationPlan::lineage_call_id`. Exhaustion fails before
`activate` is called. A wrong-type state take fails without consuming the value.

## Fiber and effects

A Fiber is one independently converging plugin authority. Its observable states
are Pending, Loading, Active, Unloading, Failed, and Disposed. Missing actual
supplies keep it Pending. Stale desired revisions or changed supply identities
cancel and roll back Loading before a fresh attempt.

Reconfiguration stages and prepares its replacement before installing a new
desired revision or retiring the current Active generation. Old and replacement
desired/attempt ownership is charged while it coexists. A staging failure leaves
the installed revision, requirement watchers, and Active generation unchanged;
a successful install atomically replaces the desired value, pending attempt, and
watchers.

Every Loading attempt owns an `EffectTxn` whose wrapper is registered before
plugin code. `defer` appends one labeled asynchronous undo, `commit` retains
the resulting LIFO effect group, and `abort` joins rollback. Dropping an open
transaction transfers abort to Runtime-owned work. Once unload begins, a new
transaction and commit fail. The setup owner of an already-open transaction
may still defer the exact undo it just acquired before abort, Drop, or the
failed commit closes that setup window. `InvocationContext::caller_effect`
exposes the same authority fenced to the exact caller generation.

`Context::provide` dynamically claims a service slot and returns a
`SupplyId`-fenced disposer. Loading occupies the slot but external lookup and
injection require Active. Listener and product registrations are immediately
visible because their undo is already owned. Supply withdrawal removes
visibility before dependent convergence and admitted-call drain.
`Context::provide_and_capture` atomically returns that disposer together with
the providing generation's own `Capability`: capability admission precedes
registry publication, so a failed capture never leaves a registered supply.
A dormant supply disposer keeps only a weak Runtime reference because its
generation effect record is already Runtime-owned. Starting disposal upgrades
that reference into task-lifetime ownership; dropping the final Runtime owner
instead drops the dormant cleanup and the registry state together, without an
ownership cycle.

## Message capabilities

`Message` contains opaque bytes and transferable `Capability` handles.
Minting accounts one unique capability entry. Cloning or transferring a Rust
handle shares that entry; it neither mints another authority nor registers
another generation-owned entry. Safe Rust exposes no raw token,
reconstruction, import operation, capability kind, or rights metadata; native
capability IDs belong to the independent ABI/Loader adapter. Retirement
immediately revokes use and removes the entry from its generation's revocation
set, while the unique entry reservation remains charged until the final safe
handle or containing Message drops. This keeps retained stale handles inside
the same memory bound and means callers release their own capabilities before
expecting a clean Runtime shutdown.
`Capability::detach` is the adapter retention seam: it consumes the strong
holder Context, preserves the exact entry and charge, and keeps only a weak
snapshot of the original holder scope. `DetachedCapability::upgrade` can
reconstruct that same holder while Runtime exists; it cannot choose a new
Context or retain activation setup authority.
Sending admits the destination channel position, byte weight, and queued
references as one operation. A sender waiting for that transaction consumes a
separate bounded pending-send reservation and holds none of those three queue
resources. Waiters have keyed cancellation and a constant per-channel
candidate window; unchanged nonfitting work is not rescanned on registration
or cancellation. Handles retain their exact generation authority until the
final owner drops. Their `Debug` output contains only logical service/provider
facts.

`Capability::open` creates a bounded bidirectional call. The caller can send
Messages, finish requests, receive Messages, cancel, and observe one explicit
terminal. Providers receive a borrowed channel. `Capability::invoke` requires
one response followed by a clean terminal and rejects every other sequence.
A cloneable `CancellationObserver` can retain only the observation of that
exact call while an adapter temporarily transfers its caller half; it cannot
request cancellation or expose the underlying token.

One absolute call deadline covers admission, request and response backpressure,
provider execution, and terminal observation. Caller and provider generations
are revalidated after admission and before driver creation. Provider panic is
call-local; retiring a provider closes new admission and waits for calls already
admitted.

Each `InvocationContext` distinguishes its own `call_id`, the immediate
`parent_call_id`, and the activation seed `lineage_call_id` for the nested
service/event chain. A first call from the activation Context has a distinct
current ID and no parent. Provider Contexts propagate lineage and current-call
facts into subsequent calls; the lineage therefore survives re-entry without
ambient thread-local state.

## Events

Registration returns an effect-owned disposer. Loading listeners are visible,
and activation rollback removes them. Dispatch snapshots listeners, evaluates
an optional `EventTarget` against immutable listener views outside locks on
blocking workers bounded by dispatch admission, and only then admits callbacks.
The absolute dispatch deadline includes selection; an over-deadline selector
starts no later selector or callback and retains its dispatch ownership until
the blocking worker returns. Explicitly global listeners bypass targeting;
selection failure starts no callbacks. Disposing an ordinary listener removes
future membership without invalidating a binding already held by one snapshot.

Emit, serial, waterfall, and parallel modes share bounded inputs, outputs,
diagnostics, callback admission, and one dispatch deadline. Parallel dispatch
admits lazily. Once claiming and every removal path share the same owner token.
One private driver contains handler-future construction, polling, iterative
outcome adoption, future and panic-payload destruction, then callback-lease
closure in that order.

## Persistent operations

Apply, reconfiguration, reconciliation, disposal, and shutdown are
Runtime-owned after admission. Deadline or caller cancellation detaches a
waiter, not the operation. Disposal and shutdown callers join the same
persistent result. Cleanup reports retain a bounded prefix plus total and
truncation metadata and cannot be mutated into a contradictory state.

The complete ownership, visibility, scheduling, and shutdown laws are defined
in the product [architecture](../docs/architecture.md). Boundary validation is
defined in [security](../docs/security.md), and public behavioral evidence in
[testing](../docs/testing.md).
