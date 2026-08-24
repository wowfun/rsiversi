# rsi-meta architecture

The core is one deep module with three public concepts:

```text
Runtime
  └─ persistent COW Context (owner + isolation + direct-edge intercept + trace)
       └─ apply(factory, config)
            └─ Fiber generation
                 ├─ dependency bindings
                 ├─ provided services and event listeners
                 ├─ child Fibers
                 └─ reverse-ordered effects
```

Internal ownership follows those contracts rather than one large facade file:
observable Runtime state is independent from mutation and scheduling, while the
service seam separates invocation identity, borrowed provider-channel
backpressure, and caller-held terminal ownership. These partitions are private;
they do not add public indirection or permit alternate lifecycle paths.

Every apply creates a Fiber. A Fiber resolves all requirements from one registry snapshot, remains `Pending` when the snapshot cannot satisfy them, and activates only after a matching publication makes convergence possible. Pending state exposes a `PendingReport` whose retained reasons and dependency-cycle service sample obey the Runtime diagnostic entry and aggregate UTF-8 byte bounds; `total_reasons` and `truncated` preserve evidence that detail was omitted. A declaration index is updated atomically with Fiber ownership and lets cycle diagnosis traverse only provider declarations reachable from the pending Fiber. Apply, reconfiguration, service changes, disposal, and shutdown submit desired revisions to one Runtime-owned reconciliation scheduler. Each Fiber has at most one queued entry and one active transition; repeated notifications coalesce to the latest revision, while independent Fibers run within the configured concurrency bound. A newer desired revision cancels a currently loading generation attempt before dependent convergence waits, so activation cannot close a transition cycle by reentrantly joining disposal of the provider whose withdrawal invalidated that attempt. A queued intent at the saturated revision remains new work even though the numeric revision cannot advance; each run captures one ticket batch and settles only that batch after its reconciliation, leaving tickets registered during the run for the queued rerun. The public resource snapshot accounts that single scheduler worker separately from active reconciliation slots, so the dense-graph probe can reject task-per-Fiber scheduling rather than inferring it from the transition limit. A transition yields its scheduler slot whenever it awaits another Fiber's reconciliation or disposal, so a concurrency limit of one cannot deadlock nested apply, reconfiguration, retirement, or explicit disposal. Public application and reconfiguration waiters follow the absolute transition-deadline and persistent-work contract in the [core README](../core/README.md): waiter timeout cannot cancel blocking preparation, an admitted transaction, rollback, or cleanup. Reconfiguration reserves an independent maximum-sized configuration staging budget before normalization and shrinks it into a configuration lease shared by the Fiber and in-flight activation, so replacement cannot release old capacity while Runtime work still owns the old allocation. Disposal linearizes by setting the Fiber's disposed fence under its data lock; it does not acquire the reconfiguration gate behind the transition lock, so a blocked normalizer cannot form a lock cycle with teardown. Setup mutations remain staged. Publication makes the generation's services and listeners visible together; setup failure disposes staged ownership without publication.

Declaration-only insertion and removal enqueue diagnostic refreshes only for
affected pending Fibers. Actual publication and withdrawal are the only service
changes that invalidate bindings or cancel a loading attempt. Once Fiber
insertion succeeds, application captures its polling executor for the ownership
handoff, so dropping that waiter on a non-Tokio thread cannot strand disposal.
The host retains that executor until Fiber disposal and Runtime shutdown finish.

Service-change dependencies and notifications are keyed by the complete service
slot: service key plus the consumer's selected isolation. Publication or
withdrawal in one isolation therefore cannot reconcile an unrelated consumer
of the same-named service in another isolation.

Scheduler slot transfer is an invariant, not a call-site optimization. Yielding
removes both the semaphore permit and its resource-ledger reservation, records
the transition as paused so the scheduler may compensate its active limit, and
reacquires both before local mutation resumes or an unwind reaches the
transition catch boundary. A nested-intent claim either completes the active
entry or requeues it on drop; a paused transition never owns a reconciliation
slot.

Retirement reverses ownership. It closes admission and withdraws publications, asks dependent Fibers to converge, waits for their binding leases and admitted callbacks, disposes children, then runs effects last-in-first-out. Cleanup and disposal are separately owned persistent runs: the first claimant transfers work to the Runtime, caller cancellation only drops a waiter, and later callers join the same bounded report. A child finishing before its parent claims cleanup transfers its already-bounded report into the owning generation as it removes the live ownership edge; this preserves descendant failures without retaining an unbounded list of completed children. Shutdown is another persistent run. Its first caller closes one Runtime-wide external-admission gate; work linearized before closure retains a lease until it finishes or transfers ownership into the Fiber registry, while later ordinary work fails without reserving resources. The shutdown driver cancels call and dispatch drivers, captures strong references to root disposal runs immediately after closure, and starts or joins each run without waiting for unrelated proof or caller-held leases; a concurrent public disposal therefore cannot remove the root identity or its report between shutdown membership capture and join. Retiring callers may still join during tracked cleanup. After disposal and scheduler convergence, shutdown atomically hard-seals the gate so neither ordinary nor retiring admission can reopen it, then drains existing leases at the final quiescence fence. Those leases can delay `Complete` but cannot head-of-line block independent cleanup. The shutdown deadline bounds one wait only; timeout returns a bounded unresolved snapshot while tracked cleanup continues, and only a sealed and drained gate, idle scheduler, zero logical resources, and empty Fiber registry produce a cached `Complete` outcome. A prior terminal reason remains diagnostic state but does not turn an otherwise quiescent teardown into a false timeout. A service handle contains both provider and caller generation identity, so a value captured from an old activation cannot silently route through a new graph. Runtime-wide service-call and buffered-byte admission remain held until the caller observes the unique terminal result or drops the call.

Every terminal disposal path settles all reconciliation tickets registered for
that Fiber, including a disposal driver that catches a late panic after the
Fiber has published its terminal snapshot.

User effect panics are contained at the effect boundary. Any panic that escapes
the cleanup transaction makes publication withdrawal unprovable, records a
bounded failure, and terminalizes the Runtime rather than permitting later
publication against possibly stale registry state.
An unexpected shutdown-driver panic is a persistent `Failed` outcome. Later
waiters receive it immediately with a fresh bounded unresolved snapshot; only
the normal quiescence fence can cache `Complete`.

Provider admission closure and its live-callback count share one atomic state.
A concurrent ordinary callback therefore either increments the count before
closure or observes the closed gate. Cleanup-time calls from an already
retiring dependent remain inside the dependent convergence transaction, which
the provider joins before atomically hard-sealing generation admission. A
retiring caller classified from stale state then either acquired before that
seal and is drained, or fails without reopening an already retired endpoint.

Listener identity has one removal transaction shared by explicit removal,
once-claiming, rollback, and unload. That transaction updates the event
registry, generation ownership, and staged state together and removes empty
event buckets. Event dispatch and callback execution have separate Runtime-wide
admission bounds. Parallel dispatch lazily admits callbacks and consumes each
outcome as it completes instead of retaining one future and result per
listener; every mode shares one absolute dispatch deadline.

The native path is an adapter chain:

```text
Loader Fiber → NativeCatalog → verified hash + mapped ABI → NativeFactory → child Fiber
```

The Loader has no privileged access to Runtime internals. Its service handler uses its own provider Context to apply children. Native, future process, and future Wasm execution therefore converge at `PluginFactory` rather than adding branches inside core.

A timed-out in-process native create or call terminalizes the complete Runtime,
not only one module. This is an intentional blast radius: trusted native code
shares memory and may already have corrupted global invariants, so a
module-local fence would claim isolation that does not exist. Process or Wasm
adapters are the route to smaller trustworthy failure domains.
