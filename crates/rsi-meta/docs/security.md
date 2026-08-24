# rsi-meta security boundary

Core trusts validated safe-Rust inputs and bounds every registry, frame,
channel, plugin configuration and overlay, activation, call, and shutdown
operation. Runtime construction validates nonzero limits, arithmetic and Tokio
primitive maxima, payload-budget relationships, and a 24-hour hard deadline
ceiling before storing a typed policy. Configured JSON depth also has a hard
ceiling of 128 so the recursive encoding library cannot follow an arbitrarily
deep safe-Rust `Value` after iterative shape validation. Both input and normalized plugin
configurations are checked for encoded bytes, JSON depth, and JSON node count
before the Runtime retains them. Owned configuration, intercept, and event
values enter an iterative-destruction guard before any fallible admission,
validation, or future construction; plugin-normalized configuration and event
handler output enter the same guard immediately on return. Rejection or an
unpolled future therefore cannot recursively destroy adversarial nesting on the
thread stack. Descriptor identity, declaration count,
dependency count, and encoded bytes are validated before cloning. A prepared
application is bound to one Runtime and holds its Fiber, declaration,
dependency, and retained-byte reservations until application or drop, so a
proof from a wider Runtime cannot bypass a stricter target Runtime.
Reconfiguration separately reserves its maximum staging configuration before
normalization. The staging reservation is shrunk into a shared Runtime-owned
configuration lease. The Fiber and every in-flight activation retain that lease
with the configuration, so old and new allocations remain included in the
aggregate retained-byte high-water mark and capacity decision for their full
coexistence. Disposal
sets the Fiber's disposed fence under the protected data lock and does not wait
for the configuration gate while holding the transition lock; an in-flight
normalizer must recheck that fence before it can publish.
Context scope construction validates service identifiers, entry count, each
retained per-service overlay encoding (including JSON-escaped service-key quotes
and list delimiters), and JSON shape before copy-on-write retention. Event registration
and dispatch validate event identifiers, and scoped dispatch validates its
service identifier, before either key reaches a Runtime registry. Service lookup
and provision validate their service and contract identifiers before ownership
resolution or staging. Cleanup-effect
labels exceeding the configured diagnostic byte bound are rejected before
retention; Runtime-generated task and listener diagnostics are UTF-8 truncated
to that bound before publication. Pending reports retain at most the configured
diagnostic reason count and aggregate identifier bytes; dependency-cycle
traversal samples no more services than those same limits allow and marks the
report truncated without first constructing the complete service path. A cycle
whose service sample has no remaining entry capacity is counted as omitted
evidence and never retained as an empty `DependencyCycle`. Plugin
errors crossing activation, service,
and event callback boundaries are normalized into a boundary-specific error
whose retained diagnostic payload obeys the same bound; formatting uses a
bounded writer rather than first cloning an arbitrarily large display value.
Cleanup reports expose bounded diagnostic state through immutable accessors;
their private representation and validated deserializer preserve the relationship
between retained failures, total failures, truncation, and `is_clean`.

Service requirements and provisions use exact contract identity and version.
Call opening uses the caller Fiber's captured time-enabled executor, so the
synchronous operation neither depends on ambient Tokio state nor probes a
missing time driver by triggering a panic. The host retains that executor until
Fiber disposal and Runtime shutdown complete.
Context ownership and Fiber generation are validated whenever a structural
operation or service call crosses the runtime seam. Service-call opening
revalidates the caller generation and exact binding after acquiring provider
admission but before channel allocation or driver spawn; a concurrent caller
transition therefore either precedes that linearization point or releases all
provisional admission through RAII. Independent preparation,
Fiber reconciliation, and live service calls have explicit Runtime-wide
concurrency limits; fail-fast operations do not hide an unbounded waiting
queue. A separate closeable Runtime admission gate covers preparation proofs,
Fiber insertion, reconfiguration, calls, dispatches, and structural Context
mutations. Shutdown closes that gate before cancellation and root discovery;
pre-close leases remain accounted while registered roots begin teardown, and
the final completion fence atomically hard-seals retiring admission after
disposal and scheduler work, then drains every existing lease. The sealed state
and acquisition share one atomic transition, so a stale retiring caller either
joins before the seal and is drained or fails without reserving resources.
Post-close attempts fail before logical resource reservation. Terminalization
is a control fence rather than ordinary external work: it remains able to record
the first bounded terminal reason and cancel drivers after shutdown has closed
the admission gate. Provider-generation retirement uses the same close then
hard-seal protocol: cleanup-time calls may join while dependents converge, but
the generation is sealed before its final callback drain. Per-call channel
limits are not treated as a global memory bound.
Queued request and response frames therefore also reserve their encoded byte
length from one Runtime-wide logical budget until the receiving side consumes
them; the budget describes retained frame bytes rather than allocator RSS.
Mixed-size waiters use bounded-bypass admission rather than a fair weighted
semaphore: a fitting frame can consume otherwise idle capacity, but no older
frame can be bypassed by more than 64 younger grants before new capacity is
reserved for it. Byte waiters hold a bounded channel slot, so this policy does
not introduce an independent unbounded queue.
Every admission authority paired with a logical ledger releases the ledger
reservation before returning capacity, so a woken waiter cannot observe
transient false capacity or violate the synchronized admission invariant.
Provider closure and its admitted-callback count are one atomic state, so an
ordinary callback racing closure either commits its count first or observes
the closed gate. Only the Runtime-owned driver owns this provider-generation
lease; caller-held terminal admission cannot extend the provider generation
after the driver publishes its unique terminal. Cleanup-time calls from retiring dependents remain ordered by
the joined convergence transaction. Caller cancellation wakes both service
halves promptly; a terminal result is published before internal cancellation,
and the caller gives an already-published terminal result and its absolute
deadline authority over that internal wake-up. Runtime-terminal and deadline
selections also remain authoritative if destroying the losing endpoint future
panics; cancellation and endpoint-result paths continue to report that panic as
`ServiceEndpointPanicked`. The driver destroys the endpoint future synchronously
before publishing its terminal or releasing provider ownership. Observing the
terminal destroys the caller response inbox and releases every queued weighted
frame reservation, including a late frame from the losing send branch. The
Runtime publishes `Complete` only after the admission gate is sealed and drained,
the scheduler is idle, every logical resource counter is zero, and the Fiber
registry is empty; later external admission cannot mutate the cached terminal
state.
An escaped cleanup-driver panic terminalizes the Runtime because publication
withdrawal is then unprovable; user effect panics remain bounded report entries.
An escaped shutdown-driver panic is cached as `ShutdownOutcome::Failed` so
later callers neither wait repeatedly nor mistake unproven cleanup for
`Complete`.
Cleanup-report wire decoding rejects contradictory retained counts and
truncation metadata before reconstructing its private byte ledger, so an
external report cannot claim a clean outcome while retaining failures.

Native plugins are trusted process code. Hashing proves which top-level bytes were mapped; it does not sandbox them or recursively authenticate operating-system dynamic dependencies. A plugin can read memory, access the operating system, spawn threads, corrupt state, or abort. Only fully trusted artifacts belong in `NativeCatalog`.

`NativeCatalog` accepts a caller-selected source path; its cache directory is a
content-addressed staging destination, not a source allowlist. Possession of the
Loader service is therefore authority to select and execute trusted native code
with the host process's permissions. Products that need an artifact policy must
place it in front of that service rather than treating the cache as a sandbox.

On Unix, the catalog pins and locks its dedicated cache directory object as the
ownership authority; `.rsi-meta.lock` remains a cooperative marker but removing
it cannot bypass the directory lock. Windows pins the pathname with a marker
handle that denies delete sharing. The catalog rejects symlinks and special
files. Unix cache enumeration, private temporary creation, digest-file opening,
no-clobber publication, post-publication comparison, rollback, and durability
fencing resolve names relative to the pinned directory descriptor. Revalidating
the public pathname only poisons a replaced Catalog and is never the authority
for cache-owned I/O. The catalog first streams a bounded regular file through a fixed-size
hash buffer; a matching live digest reuses its already verified mapping without
claiming another staging artifact. A cold identity is read again into a private
stable copy while hashing, and that second digest is authoritative if the source
changed between reads. Aggregate durable-cache and live-staging budgets are
reserved before file creation or commitment. The catalog maps the private copy,
commits a digest only after ABI and descriptor validation, recomputes that
digest while streaming the commit copy, and never reopens a durable path as
mapping authority. Later replacement or in-place mutation of the durable cache
path therefore cannot change staged content or publish modified staging under
an old digest. On Linux, cache accounting commits only after the published
directory entry passes its durability fence. A failed fence is rolled back by
removing the entry relative to the pinned directory and durably syncing that
same directory, so that rollback never targets a replacement-owned entry.
Failure to establish either cleanup step poisons the Catalog, and all later
load admission fails closed. The private
descriptor remains writable to trusted code with the host process's authority;
this does not weaken the stated trust boundary, because native plugins can
already mutate arbitrary process resources. Windows seals the private staged
artifact by reopening it read-only, rechecking its digest, and denying write
and delete sharing until unload; any Windows support claim requires a
passing native conformance job and is never inferred from Unix evidence.
Native outbound service identifiers and request lengths are rejected at the
Runtime identifier/frame limits before the host borrows their ABI pointers.
The dedicated cache accepts only its lock and canonical digest files at
startup; stale staging or unmanaged entries require operator cleanup while the
directory is unlocked instead of creating unbounded scan history.
Timed-out load workers and live factories or instances retain the cache lease
through foreign teardown, so an unlocked directory never contains staging
still owned by native execution. A mapped factory reserves finalizer admission
before native entry executes, so ordinary destruction saturation cannot strand
its mapping or cache lease. Each create separately reserves a bounded
live-instance slot before its callback thread is spawned and retains that slot
through real destruction. Publication failure or direct Runtime-owner release
transfers the reservation and instance resources to a physically bounded
reserved queue. Those paths therefore need neither inline foreign destruction,
an unbounded hidden backlog, nor task-per-instance spawning.

The ABI validates table sizes, versions, mandatory pointers, returned-buffer structure, output bounds, UTF-8/JSON at owning boundaries, and allocator ownership. An allocator-matched guard releases every plugin-returned buffer exactly once after either copying or rejection. Every unsafe operation in `rsi-meta-plugin` and `rsi-meta-loader` documents pointer, lifetime, serialization, or mapping requirements. Core contains no unsafe code.

A native callback that exceeds its deadline cannot be forcibly stopped safely,
so the adapter retains its thread, library, gate, and accounted resources until
foreign code actually exits. Timeout authority is operation-scoped: create and
service-call callbacks atomically publish poison and the owning Runtime's
terminal fence; pre-application configuration validation has no Runtime
authority and poisons only its factory; catalog load owns a digest fence and
cache lease rather than a Runtime; admitted destruction has no adapter-local
deadline. The authoritative callback, load, destruction, and concurrency
contract is the [loader README](../loader/README.md). The Runtime terminal fence
bounds later adapter admission but cannot constrain arbitrary resources created
by trusted native code.
Process or Wasm adapters may offer stronger termination without changing core.
