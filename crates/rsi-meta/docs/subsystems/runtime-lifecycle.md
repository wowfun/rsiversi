# Runtime lifecycle

## Context and ownership

Execution is an explicit bootstrap dependency, defined below core, for owned
tasks, preparation jobs and monotonic deadlines. Core does not select an executor
per Fiber or per caller. Platform adapters supply native Tokio or browser Worker
execution; neither adapter adds a composition graph. Native constructors may
capture an explicitly entered Tokio runtime as a convenience. Drop paths and an
empty Runtime retain the same execution authority as ordinary active Fibers.

The browser adapter accepts only trusted bounded synchronous preparation. The
deadline includes that work but cannot interrupt it; stale results cannot publish
after control returns. Native unwind containment remains intact. On panic-abort
WASM, a trap destroys the Worker and cannot report cleanup or shutdown completion.

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

`Context::retirement_observer` captures an observation-only signal for the exact
live generation, or Runtime admission for a root Context. It fires when that
owner closes admission, before draining calls, children, or deferred cleanup.
It retains neither execution admission nor the Runtime. Local adapters use it
to release suspended outbound calls that their later deferred cleanup must join;
the signal grants no cleanup authority and does not replace owned cleanup.



## Preparation, injection, and activation

`ResolvedFactory` contains one already bounded `FactoryIdentity`, static
`UpdateMode`, and the `PluginFactory` implementation. `into_parts` lets a trusted resolver consume that value to wrap its implementation
while retaining provenance and policy; it performs no execution or attestation.
The Runtime validates and
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
An inserted apply retains its disposal guard through the final deadline check.
Only acceptance of the result transfers that responsibility to the returned
Fiber handle; a late successful activation is still disposed when its result is rejected.



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

`Context::registration_context` issues a narrow Local registrar credential for
one exact non-root generation. It cannot apply children, resolve dependencies,
provide services, or impersonate another Context. Local registrars install exact
undo through this credential before publishing a contribution. Loading joins
the existing setup transaction; Active uses one dynamic effect. Publication
validates the current generation and serializes with exact removal. The bounded
publication closure only mutates the owning registrar and never invokes plugin
callbacks or waits. Business callbacks run after capturing a snapshot and
releasing all registrar and lifecycle locks.

The returned registration lease closes new admission immediately when disposed
or dropped. Generation retirement uses the same exact removal, and retained
registration tokens observe that closure. Registrars check token liveness when
capturing new work; an already captured dispatch keeps its documented snapshot
semantics. Synchronous undo is contained and owned by the same effect record,
including failure reporting. Registration order tokens retain position and an
owner-local ordering tie-break, without granting mutation authority.

`InvocationContext::caller_effect` lets a service implement an operation on
behalf of its exact caller generation. Contributions made through that handle
retire with the caller, not the provider. The handle is generation-fenced and
cannot register after its owner transaction closes.



## Read-only inspection

`Runtime::inspect` returns a bounded, owned page of redacted Fiber metadata;
`Context::inspect` restricts membership to its owning Fiber and descendants and
fences a retired generation. A root Context observes the Runtime. Neither entry
point prepares plugins, invokes callbacks, mutates registries, or exposes config,
opaque state, service values, effect labels or raw failure/terminal diagnostics.
The page includes exact factory provenance, lifecycle kind, parent generation,
actual composition order, prepared requirements with captured provider bindings,
owned supplies, effect state/counts and listener/child counts. Only whole-Runtime
inspection includes the existing global resource snapshot.

Inspection accepts at most 64 Fibers and 128 items per Fiber collection. Each
collection reports its total as well as its retained prefix. An exclusive Fiber
ID cursor pages membership in ID order; clients use the captured order paths for
contribution order. Registry membership is captured under its lock, Fiber data
outside that lock, and effect/order observations outside Fiber locks. This is an
operational observation across those boundaries, not a transactional graph or a
proof of cleanup quiescence. Callers own retention of returned pages. Bounded
identifiers and provenance remain observable metadata, not secret storage.
Effect-table counts exclude records already transferred to the cleanup driver;
the generation's existing effect budgets and cleanup phase report that retained
work separately. A supply records whether its generation reached publication,
not a guarantee that an observed supply remains callable.



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
